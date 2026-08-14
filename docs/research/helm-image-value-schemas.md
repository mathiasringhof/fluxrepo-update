# Helm Image Value Schemas

Research date: 2026-08-14

This note surveys official values files for seven widely used Helm charts. The goal is
not to recognize arbitrary keys named `tag` or `version`; it is to identify schemas that
provide a safe Image Binding between an Explicit Version and a public container image.

## Representative schemas

| Chart | Exact value paths and patterns | Safe static interpretation |
| --- | --- | --- |
| Bitnami PostgreSQL | `image.{registry,repository,tag,digest}`, `volumePermissions.image.{registry,repository,tag,digest}`, and `metrics.image.{registry,repository,tag,digest}` | A recursively nested `repository` + `tag` pair is an Image Binding. Prepend `registry` when present. A non-empty `digest` takes precedence over the tag. [Official values](https://github.com/bitnami/charts/blob/main/bitnami/postgresql/values.yaml) |
| Grafana | `image.{registry,repository,tag,sha}`, `testFramework.image.{registry,repository,tag}`, `downloadDashboardsImage.{registry,repository,tag,sha}`, `initChownData.image.*`, `sidecar.image.*`, and `imageRenderer.image.*` | The same recursive mapping rule covers the main image and auxiliary containers. A non-empty `sha` pins the artifact. `imageRenderer.image.tag` currently demonstrates why mutable values such as `latest` must be skipped. [Official values](https://github.com/grafana-community/helm-charts/blob/main/charts/grafana/values.yaml) |
| kube-prometheus-stack | Repeated `{registry,repository,tag,sha}` maps include `upgradeJob.image.busybox`, `upgradeJob.image.kubectl`, `alertmanager.alertmanagerSpec.image`, `prometheusOperator.image`, `prometheusOperator.prometheusConfigReloader.image`, `prometheus.prometheusSpec.image`, and `thanosRuler.thanosRulerSpec.image` | Recursive recognition matters more than enumerating parent paths: the same self-contained image map appears at several depths and under subchart values. Blank tags inherit another value and are not Explicit Versions. [Official values](https://github.com/prometheus-community/helm-charts/blob/main/charts/kube-prometheus-stack/values.yaml) |
| ingress-nginx | `controller.image.{registry,image,repository?,tag,digest,digestChroot}`, `controller.admissionWebhooks.patch.image.{registry,image,repository?,tag,digest}`, and `defaultBackend.image.{registry,image,repository?,tag}`; the shared registry is `global.image.registry` | This chart uses `image`, rather than `repository`, for the repository path. It needs a chart-specific `{image,tag}` adapter because treating every sibling pair with those names as a generic schema would be ambiguous. A non-empty digest blocks a tag-only update. [Official values](https://github.com/kubernetes/ingress-nginx/blob/main/charts/ingress-nginx/values.yaml) |
| Argo CD | `global.image.{repository,tag}` supplies the shared Argo CD image. Component maps such as `controller.image`, `server.image`, `repoServer.image`, `applicationSet.image`, and `notifications.image` may contain empty overrides that fall back to it. Independent images include `dex.image.{repository,tag}` and `redis.image.{repository,tag}`. | A concrete `global.image.repository` + `global.image.tag` is a single shared Update Target. Empty component overrides are not Explicit Versions and must not become duplicate targets. Independent concrete mappings still follow the generic rule. [Official values](https://github.com/argoproj/argo-helm/blob/main/charts/argo-cd/values.yaml) |
| cert-manager | Root `imageRegistry` and `imageNamespace` combine with `image.name`; the pattern repeats at `webhook.image`, `cainjector.image`, `acmesolver.image`, and `startupapicheck.image`. Each component also supports a full `image.repository` override and optional `image.tag` and `image.digest`. | A concrete repository override + tag follows the generic rule. A tag next to only `name` requires a cert-manager adapter to construct `imageRegistry/imageNamespace/name`. Omitted tags default to chart `appVersion` and are not targets. [Official values](https://github.com/cert-manager/cert-manager/blob/master/deploy/charts/cert-manager/values.yaml) |
| GitLab | `global.gitlabVersion` supplies the default image tag for webservice, sidekiq, and migrations; `global.image.tagSuffix` modifies tags globally. Gitaly, GitLab Shell, and GitLab Runner have separate compatibility requirements. Some init containers use `init.image.tag` with a repository inherited from `global.gitlabBase.image.repository`. | `global.gitlabVersion` is a chart-specific shared Image Binding, not evidence that arbitrary `*Version` keys are images. Supporting it safely requires a GitLab adapter and awareness that it does not update every bundled component. [Official global-values documentation](https://gitlab.com/gitlab-org/charts/gitlab/-/blob/master/doc/charts/globals.md) and [official values](https://gitlab.com/gitlab-org/charts/gitlab/blob/master/values.yaml) |

## Comparison with the current scanner

[`src/scanner.rs`](../../src/scanner.rs) already walks mappings and sequences recursively.
Its `collect_images` function recognizes two forms:

- a scalar under any key named `image`, such as `image: quay.io/acme/app:v1.2.3`;
- a mapping under `image` with a `repository` and optional `tag`, from which it
  synthesizes a scalar reference.

This provides useful inventory coverage, but it is not yet sufficient for updating Helm
values:

- Only image fields in `Deployment.spec.template.spec.containers[*]` and
  `initContainers[*]` become Update Targets. Every Helm values image is inventoried only.
- A mapping target records the path of the outer `image` mapping, not the path of its
  version-bearing `tag` scalar. The updater therefore lacks the edit location required
  for a tag-only change.
- `registry` is ignored. This happens to work for Docker Hub repositories such as
  `grafana/grafana`, but misidentifies values such as
  `registry: quay.io` plus `repository: prometheus/prometheus`.
- `digest`, `sha`, and ingress-nginx's `digestChroot` are ignored. Updating the tag while
  leaving a digest pin intact may not change the deployed artifact.
- On encountering an `image` mapping without `repository`, recursion stops at that
  mapping. Consequently ingress-nginx's nested `image` + `tag` form is not discovered,
  and tag-only shared or constructed forms are also absent.
- Empty tags that inherit `Chart.appVersion` are rendered as bare repositories. That is
  appropriate for inventory, but such inherited values must not become Explicit Version
  targets.
- The file walker accepts `.yaml` but not `.yml`. It correctly fails on malformed YAML,
  skips hidden/cache directories, and excludes generated `flux-system/gotk-*` manifests.

## Safe recognition recommendations

Implement recognition as a small set of ordered schema rules. Each recognized candidate
should retain separate paths for the version scalar and the values used to construct its
Version Source.

1. **Full scalar image reference.** Recognize a scalar `image` value only when it contains
   a concrete repository and explicit, immutable-looking tag. Keep digest-pinned,
   tagless, templated, and mutable-tag references unchanged and report why they were
   skipped.
2. **Generic image mapping.** At any depth under `HelmRelease.spec.values`, recognize a
   mapping with non-empty scalar `repository` and `tag`. Build the repository from
   optional `registry` plus `repository`, avoiding a duplicate registry when
   `repository` is already fully qualified. This rule covers Bitnami, Grafana, most of
   kube-prometheus-stack, and independent Argo CD images without enumerating subchart
   prefixes.
3. **Digest guard.** If the same mapping has a non-empty `digest` or `sha`, do not propose
   a tag-only update. `digestChroot` should receive the same treatment in the
   ingress-nginx adapter. Digest refresh is a separate feature because registry tags and
   digests have different resolution and mutation semantics.
4. **Narrow chart adapters.** Dispatch by the HelmRelease chart identity, then recognize
   only documented paths: ingress-nginx's `{image,tag}` maps, Argo CD's
   `global.image.{repository,tag}`, cert-manager's constructed repository, and GitLab's
   `global.gitlabVersion`. These rules must not broaden into generic `image` + `tag`,
   `*Version`, or tag-only matching.
5. **Explicit values only.** Ignore blank or omitted tags that inherit chart
   `appVersion`, and ignore component fields that merely inherit a global tag. A shared
   concrete field is one Update Target even when it controls several rendered
   containers.
6. **Best-effort discovery.** Unknown mappings remain unchanged and can be reported as
   potential Unresolved Targets. New recurring chart schemas can be added as built-in
   adapters when concrete repository examples demonstrate the need; no user
   configuration format is required yet.

The conservative first implementation should therefore support scalar references,
recursive concrete `repository` + `tag` mappings, and the digest guard. The four
chart-specific adapters are useful follow-ups, ordered by observed repository needs,
rather than prerequisites for the generic scanner.
