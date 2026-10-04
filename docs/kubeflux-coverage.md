# kubeflux coverage audit

Audited on 2026-10-04 against clean kubeflux commit
[`f5459d8`](https://github.com/mathiasringhof/kubeflux/tree/f5459d8e0f1b7f3ba5dbc7559f9fc83b70235034)
and updater `951a51f`. Paths below refer to that kubeflux snapshot, including inactive
bases, diagnostics, and lab fixtures. This audits declarations and test coverage, not
live update availability or deployed resources. No updater features were changed.

The opt-in [synthetic corpus](../coverage/kubeflux/README.md) exercises the CLI using
local HTTP fixtures; the [status table](../coverage/kubeflux/STATUS.md) owns recorded
results and links every case. A PASS can mean safe preservation; TODOs retain desired
behavior that currently fails. U1–U15 remain open. T1–T4 identify regression examples,
not a claim of complete update support.

## Inventory baseline

Of 573 tracked YAML files, six generated Flux files are excluded. There are 63
HelmReleases (31 without a local chart version), 25 HelmRepositories, and three groups
of equivalent repeated sources. The scanner reports **98 Version Declarations**:

| Declaration | Count | Breakdown |
| --- | ---: | --- |
| Explicit chart versions | 32 | Local chart/source identity is present; resolution can still fail. |
| Image bindings | 51 | 13 versioned, 24 digest-pinned, 8 mutable, 4 tagless, 1 commit tag, 1 unversioned flavor. |
| Unchecked declarations | 15 | 6 tag-only Helm overrides, 2 CloudNativePG images, 7 HTTP(S) Kustomize resources. |

These are recognized declarations, not all dependencies. Silent misses appear in neither
`discovered_count` nor `unchecked_count`. See [product coverage](coverage.md) for counting,
resolution, and write semantics.

Existing Rust evidence covers HTTP/OCI charts, all six workload kinds and both container
lists, recursive Helm values, repeated sources, unchecked declarations, and preservation:
[workflows](../tests/rust_workflows.rs), [scanner](../tests/rust_scanner.rs),
[resolvers](../tests/rust_resolvers.rs), and [update runs](../tests/rust_update_run.rs).
Real examples include audiobookshelf/TrueCharts charts, sonarr Deployment, kube-vip
DaemonSet, mediabackup CronJob, hermes Jobs/StatefulSet, Jellyfin image lists, and nginx
extra containers. The corpus makes these shapes and the following gaps independently
executable.

## Open gaps

- [ ] <a id="u1"></a>**U1 — Effective Kustomize namespaces** ([#16](https://github.com/mathiasringhof/fluxrepo-update/issues/16)).
  `apps/{production,testing,apptesting}/jellyfin/release.yaml` references namespace
  `jellyfin`; raw `repository.yaml` uses `flux-system`, transformed by each
  `kustomization.yaml`. Exercise equivalent sources across transformed environments
  together, and keep genuinely conflicting sources unresolved.
- [ ] <a id="u2"></a>**U2 — Inherited identity and chart defaults.** Six tag-only overrides:
  two in `apps/base/immich/release.yaml`, one each in
  `infrastructure/{production,testing}/openebs/release.yaml`,
  `apps/base/ente-auth/release.yaml`, and `apps/apptesting/ente-auth/release-patch.yaml`.
  Values-only overlays also occur in `apps/production/audiobookshelf/release-patch.yaml`.
  Test base/overlay provenance and unresolved defaults; preserve tag-only digest pins
  and opaque Secret `valuesFrom`. A missing/blank version is not proof of currency.
  The unknown-default preservation guard does not expand chart contents.
- [ ] <a id="u3"></a>**U3 — CloudNativePG images.**
  `apps/{production,apptesting}/immich/cloudnative-pg.yaml` uses
  `Cluster.spec.imageName: ghcr.io/tensorchord/cloudnative-vectorchord:16.9-0.4.3`.
  Test located discovery and updates within an explicit PostgreSQL/extension version
  family, with cross-family candidates and unrelated `Cluster` API groups excluded.
  Selection alone cannot establish upgrade compatibility.
- [ ] <a id="u4"></a>**U4 — Kustomize remote pins.** Seven HTTP(S) resources are unchecked:
  two each in `infrastructure/base/{cdi,kubevirt}/kustomization.yaml`, three in
  `infrastructure/base/intel-gpu/kustomization.yaml`. Two scheme-less
  `github.com/metallb/metallb/config/native?ref=v0.15.3` entries in
  `infrastructure/{base,testing}/metallb/kustomization.yaml` are missed.
  Exercise release URLs and both ref spellings; coordinated resources must select one
  release and retain each subdirectory/artifact path. Exclude local paths and comments.
- [ ] <a id="u5"></a>**U5 — Digest/channel/commit updates.** Of 51 image bindings, 38 cannot
  currently be checked: 24 digest pins, 8 mutable, 4 tagless, `sha-4fd4faa`, and `arch-kde`.
  Sources include hermes, zfsreplication, ente/Valkey `image.tag` digests, smokeping,
  loki/syslog-ng, tvproxy's tagless host:port, zfs_exporter, and webtop-privacy.
  Test visibility and preservation of each form. Future digest refresh/channel policies
  must retain pins, avoid invented tags, and coordinate repeated references.
- [ ] <a id="u6"></a>**U6 — Stable release selection** ([#15](https://github.com/mathiasringhof/fluxrepo-update/issues/15)).
  Three Jellyfin images can select unstable timestamp builds. Given current `10.11.8`
  and candidates `10.11.8`, `12.1`, `2026092811`, prefer the stable semantic release;
  retain valid calendar-to-calendar updates. Chart rules must also handle
  cert-manager `v1.21.1`, rke2-multus `v4.3.017`, and unpoller `2.11.2-Chart6` (T3).
- [ ] <a id="u7"></a>**U7 — Post-renderer patch strings.**
  `apps/apptesting/immich/release-patch.yaml` embeds `busybox:1.36` in
  `spec.postRenderers[0].kustomize.patches[0].patch`, a JSON6902 init-container add.
  Discover both an adjacent ordinary image and the patch image with distinct outer/inner
  locations. Initially report the latter unchecked; preserve and validate both YAML layers.
- [ ] <a id="u8"></a>**U8 — Image-valued environment variables.**
  `apps/base/zfsreplication/deployment.yaml` repeats its controller digest in
  `env[RELEASE_IMAGE].value`. `DOCKER_MODS` has three tokens in
  `apps/production/openssh/deployment.yaml`, one in `apps/production/webtop/deployment.yaml`,
  and one in `apps/apptesting/webtop-privacy/deployment.yaml`.
  Test single/pipe-separated tokens with distinct positions and arbitrary text as a
  negative control. Coordinated references must stay consistent before edits are enabled.
- [ ] <a id="u9"></a>**U9 — VM image sources.** Eight `DataVolume.spec.source.http.url`
  declarations occur in `kubevirt/production/vms/{k3s-apptesting,haos,wgvm}/rootdisk-dv.yaml`,
  `kubevirt/production/lab/vms/resources.yaml` (three documents), and
  `kubevirt/testing/{wgvm-ubuntu-import-dv,vm-102-import-dv}.yaml`.
  Five use Ubuntu `noble/current`; three are opaque private imports. ISO paths occur in
  `kubevirt/{apptesting,testing}/ubnt3-autoinstall-test.yaml` (`24.04.4`) and
  `kubevirt/testing/debian-vm/debian.yaml` (`13.1.0`). Test provenance/unknown availability;
  exclude VM IDs and ordinary disk paths, and require a provisioned replacement before
  rewriting a local ISO path.
- [ ] <a id="u10"></a>**U10 — Executable content.**
  `kubevirt/production/lab/runner/run.py` downloads K3s `v1.35.8+k3s1` plus its checksum
  file and builds `busybox:1.37.0` Pods; `directrouting.py` builds digest-pinned Python
  Pods. ConfigMap generators package these scripts; runner/gate scripts assert versions.
  `scripts/validate.sh` downloads Flux schemas through `releases/latest/download`.
  Test generated resources, coupled downloads, and mutable URLs with source locations,
  without executing code or treating validation assertions as additional installs.
  Unpinned cloud-init/install scripts and package-manager commands have unknown runtime
  versions, not editable version declarations.
- [ ] <a id="u11"></a>**U11 — Tool/package pins.** `tests/requirements.txt` pins
  `PyYAML==6.0.3` and `cel-python==0.5.0`; `scripts/lab/guest.py` pins virtctl by checksum
  without a release identity. Report these through an explicit scope extension or
  delegate to a package/tool updater; never infer a release from a checksum.
- [ ] <a id="u12"></a>**U12 — Bundles and Flux bootstrap.**
  `apps/apptesting/kubevirt-manager/bundled-v1.5.3.yaml` contains image `1.5.4` but
  `1.5.3` bundle labels/selectors. Six generated `clusters/*/flux-system/gotk-*` files
  contain twelve controller images and Flux `2.7.5`, `2.8.5`, `2.8.6`.
  Track bundle/bootstrap upgrades separately from image updates. Generated files must
  remain excluded from automatic edits and byte-identical even when another image updates.
- [ ] <a id="u13"></a>**U13 — Completeness and failed resolution** ([#18](https://github.com/mathiasringhof/fluxrepo-update/issues/18)).
  Recognized counts separate checked/unchecked declarations but omit silent misses.
  Exercise current, all-failed, mixed, unsupported-only, and undiscovered dependencies.
  Exit zero or zero planned updates must not imply repository-wide currency.
- [ ] <a id="u14"></a>**U14 — Grafana dashboards.**
  `apps/{production,apptesting}/kube-prometheus-stack/kube-prometheus-stack-values.yaml`
  pins `spec.values.grafana.dashboards.default.smartctl_exporter` with `gnetId: 22604`
  and `revision: 2`. Production has five unpoller URLs ending `/revisions/latest/download`.
  Discover revision pins and unknown channel availability; preserve dashboard identity
  and exclude `gnetId` and unrelated numeric/revision fields from version selection.
- [ ] <a id="u15"></a>**U15 — Applying indentless lists.** Valid sibling block sequences
  can plan successfully but fail apply with a false missing-path/changed-target error.
  The workload and Helm-values cases require all selected images to update while
  preserving style and neighbors. This is a writer bug exposed by the corpus, not an
  additional kubeflux dependency category.

## Regression examples

- [x] <a id="t1"></a>**T1 — Helm values lists/scalars/mappings.** Jellyfin/nginx
  `initContainers[]` and `extraContainers[]`, nested Immich Valkey, shared tags, and
  optional registries: exact indexed locations, selected updates, and unchanged neighbors.
- [x] <a id="t2"></a>**T2 — Conservative image forms.** Digest-only, tag+digest, mapping
  digests, tag-only pins, `main`, `arch-kde`, `sha-4fd4faa`, and tagless host:port: one
  declaration per field, exact skip reason, and no network-dependent classification.
- [x] <a id="t3"></a>**T3 — Version selection.** Webtop `20260830`, sonarr
  `version-4.0.19.2979`, OpenSSH `version-10.3_p1-r0`, and the U6 charts: select via local
  registry/index responses, preserve published spellings/suffixes, and reject downgrades.
- [x] <a id="t4"></a>**T4 — Negative controls.** SOPS/schema/API/protocol versions,
  network-data `version: 2`, rollout revisions, arbitrary numbers, comments, and ordinary
  disk paths stay unchanged beside an actual update. Include inactive uptimekuma bases
  while preserving excluded paths and opaque Secret references.

No user-authored `chartRef`, `OCIRepository`, Kustomize `images` transformer, or CI workflow
was found. These remain broader product candidates. Secrets, chart defaults, remote
resources, and downloaded scripts were not expanded; there is no transitive coverage claim.

To refresh: record both commits and working-tree status, run `inventory --json` against
kubeflux, independently inspect YAML and non-YAML dependency shapes, then map each to a
case or explicit boundary. Run the corpus and CI checks. Use minimal anonymous fixtures;
do not copy full manifests, encrypted secrets, live responses, or the checkout into tests.
