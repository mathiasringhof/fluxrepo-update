# kubeflux coverage audit

Audited on 2026-10-04 against clean kubeflux commit
[`f5459d8`](https://github.com/mathiasringhof/kubeflux/tree/f5459d8e0f1b7f3ba5dbc7559f9fc83b70235034)
and fluxrepo-update `951a51f`. Paths below are relative to that kubeflux snapshot.
This is a declaration and test-coverage audit, not a live check for newer releases.
Inactive bases, diagnostics, and lab fixtures are included; inclusion does not imply deployment.

## Run the synthetic examples

The [opt-in corpus](../coverage/kubeflux/README.md) exercises these categories through
actual CLI calls against temporary repositories and local HTTP fixtures. It is separate
from `cargo test` and CI; the existing Rust tests are unchanged.

```sh
cargo build --locked
python3 coverage/kubeflux/run.py
python3 coverage/kubeflux/run.py --case helm-values-lists
python3 coverage/kubeflux/run.py --strict
```

The [2026-10-04 results](../coverage/kubeflux/STATUS.md) record **73 cases: 44 PASS,
29 TODO, and no FAIL, ERROR, or XPASS**. TODO cases retain the desired behavior and
expose current gaps; they do not count as supported updates. A preservation PASS proves
safe skipping, not a check for newer versions. U1–U15 remain open; T1–T4 now have
executable examples. No updater features were changed.

## Inventory baseline

The snapshot has 573 tracked YAML files, including six excluded generated Flux files.
The scanner reports **98 Version Declarations**:

| Declaration | Count | Meaning |
| --- | ---: | --- |
| Explicit chart versions | 32 | Manifest-local chart/source identity is present; resolution can still fail. |
| Image bindings | 51 | 13 versioned, 24 digest-pinned, 8 mutable, 4 tagless, 1 commit tag, 1 unversioned flavor. |
| Unchecked declarations | 15 | 6 tag-only Helm overrides, 2 CloudNativePG images, 7 HTTP(S) Kustomize resources. |

There are 63 HelmReleases (31 lack a local chart version) and 25 HelmRepository
declarations. Three repeated source groups are equivalent; none is ambiguous.
These counts describe recognized declarations, **not all dependencies**. Silently missed
declarations contribute to neither `discovered_count` nor `unchecked_count`; declarations
already reported as unchecked do count.

## Categories and coverage evidence

Existing Rust tests are linked by file; names identify the behavioral case. The
[corpus status table](../coverage/kubeflux/STATUS.md) links every additional executable
example to its audit item. A tested skip is coverage of the current boundary, not
evidence that update availability was checked.

| Category | kubeflux example | Existing evidence / remaining gap |
| --- | --- | --- |
| HTTP and OCI Helm charts | `apps/base/audiobookshelf/release.yaml`; `infrastructure/base/sources/truecharts.yaml` | [Workflows](../tests/rust_workflows.rs): `update_helm_workflow_plans_and_applies_from_local_remotes`; [resolvers](../tests/rust_resolvers.rs): `repository_chart_resolver_resolves_generic_oci_repositories_by_protocol`. |
| Repeated chart sources | backube, jellyfin, openebs-zfs across environments | [Update Run](../tests/rust_update_run.rs): `equivalent_source_copies_allow_each_manifest_to_update`, `differing_source_specs_or_namespaces_remain_ambiguous`. Namespace transforms remain TODO U1. |
| Values-only and incomplete overlays | `apps/production/audiobookshelf/release-patch.yaml` | [Scanner](../tests/rust_scanner.rs): `scanner_keeps_values_only_overlays_out_of_chart_targets`, `scanner_does_not_inherit_chart_identity_for_patch_versions`. No cross-file inference; U2. |
| Workload containers and init containers | sonarr Deployment, kube-vip DaemonSet, mediabackup CronJob, hermes Jobs | [Workflows](../tests/rust_workflows.rs): `update_helm_workflow_applies_all_standard_podspec_image_bindings` covers all six supported kinds and both container lists. |
| Recursive Helm image scalars and mappings | Jellyfin `image.repository/tag`; Immich nested Valkey mapping | [Workflows](../tests/rust_workflows.rs): `update_helm_workflow_updates_recursive_mapping_and_scalar_helm_values`; shared tags and optional registry also have scanner tests. List examples are implemented under T1; U15 records an apply failure. |
| Tag-only Helm overrides | Immich, OpenEBS, ente-auth | [Workflows](../tests/rust_workflows.rs): `tag_only_image_overrides_are_visible_and_never_written`; visible but unchecked, U2. |
| CloudNativePG images | `apps/{production,apptesting}/immich/cloudnative-pg.yaml` | [Workflows](../tests/rust_workflows.rs): `cloudnativepg_images_are_visible_without_enabling_new_writes`; U3. |
| Remote Kustomize resources | CDI, KubeVirt, Intel GPU; scheme-less MetalLB | [Workflows](../tests/rust_workflows.rs): `remote_kustomize_resources_are_reported_without_fetching_or_editing_them` covers HTTPS release and ref URLs. Scheme-less refs are missed; U4. |
| Digest, mutable, tagless, commit, and flavor images | hermes, zfsreplication, smokeping, tvproxy, zfs_exporter, webtop-privacy | [Resolvers](../tests/rust_resolvers.rs): `registry_resolver_rejects_unsupported_image_references_before_network`; [workflows](../tests/rust_workflows.rs): `update_helm_workflow_reports_conservative_helm_value_skips`. T2 now exercises the exact forms; updates remain U5. |
| Version families and suffixes | sonarr `version-4.0.19.2979`, openssh `version-10.3_p1-r0`, webtop `20260830` | [Resolvers](../tests/rust_resolvers.rs): `newer_version_supports_linuxserver_style_tags`, `image_tag_variants_preserve_flavor_and_architecture`, `version_comparison_stays_within_comparable_families`. T3 exercises real selection; U6 remains open. |
| Inline patch images | Immich post-renderer init container | No discovery; executable TODO [U7](#u7). |
| Images in environment values | zfsreplication `RELEASE_IMAGE`; openssh/webtop `DOCKER_MODS` | No discovery; executable TODO [U8](#u8). |
| VM disks, ISO versions, and image channels | KubeVirt VirtualMachine and CDI DataVolume | No discovery; executable TODO [U9](#u9). |
| Script-generated resources and downloaded binaries | lab runner ConfigMap source scripts | Non-YAML source and embedded code are not inspected; executable TODO [U10](#u10). |
| Tool/package pins | `tests/requirements.txt`; virtctl checksum | Outside discovery; executable TODO [U11](#u11). |
| Grafana dashboard revisions and channels | smartctl dashboard revision; unpoller dashboard URLs | No discovery; executable TODO [U14](#u14). |
| Vendored bundles and generated Flux | kubevirt-manager bundle; six `gotk-*` files | Ordinary bundle image is found; bundle refresh is not. [Workflows](../tests/rust_workflows.rs): `update_helm_write_workflow_keeps_unsupported_and_generated_files_untouched`; U12. |
| Scope and reporting | inactive uptimekuma base; skipped declarations | [Scanner](../tests/rust_scanner.rs): `inventory_reports_unchecked_declarations_and_includes_inactive_manifests`; [workflows](../tests/rust_workflows.rs): `coverage_counts_include_current_and_unchecked_declarations_in_plan_and_apply`. T4 examples preserve exclusions; U13 remains open. |

## TODO: unsupported updates and known failures

Each open item has executable examples in the [corpus](../coverage/kubeflux/STATUS.md).
The desired checks specify discovery, updates, or preservation as appropriate; passing a
preservation guard does not implement the feature. These examples do not enable new writes.

- [ ] <a id="u1"></a>**U1 — Effective Kustomize source namespaces** ([#16](https://github.com/mathiasringhof/fluxrepo-update/issues/16)).
  Jellyfin releases in `apps/{production,testing,apptesting}/jellyfin/release.yaml`
  reference `jellyfin`, while raw sources use `flux-system` and Kustomize transforms
  both into `jellyfin`. Synthetic: two environments with equivalent sources and a
  namespace transform resolve correctly; a genuinely conflicting source stays unresolved.
- [ ] <a id="u2"></a>**U2 — Inherited chart/image identity and defaults.** Six explicit `image.tag`
  overrides lack local repositories: two in `apps/base/immich/release.yaml`, one each
  in `infrastructure/{production,testing}/openebs/release.yaml`, and one each in
  `apps/base/ente-auth/release.yaml` and `apps/apptesting/ente-auth/release-patch.yaml`.
  Synthetic: base plus tag-only overlay and chart-default image; retain provenance and
  report unknown identity until it can be established. Preserve tag-only digest pins.
  Missing/blank versions are not proof of current dependencies. Secret `valuesFrom`
  references are opaque; no plaintext image override was observed there. The
  [unknown-default guard](../coverage/kubeflux/cases/chart-default-image-unknown/case.json)
  proves explicit overrides stay visible and unchanged; it does not expand unavailable
  chart contents or discover dependencies hidden in chart defaults.
- [ ] <a id="u3"></a>**U3 — CloudNativePG `Cluster.spec.imageName`.** Two Immich manifests use
  `ghcr.io/tensorchord/cloudnative-vectorchord:16.9-0.4.3`. Synthetic: report a known
  newer image through its exact field under an explicit PostgreSQL/extension version
  selection policy, without promising upgrade compatibility; exclude unrelated `Cluster`
  API groups.
- [ ] <a id="u4"></a>**U4 — Kustomize remote version pins.** Seven HTTP(S) entries are reported but
  unchecked: two each in `infrastructure/base/{cdi,kubevirt}/kustomization.yaml` and
  three in `infrastructure/base/intel-gpu/kustomization.yaml`. Two
  `github.com/metallb/metallb/config/native?ref=v0.15.3` entries in
  `infrastructure/{base,testing}/metallb/kustomization.yaml` are silently missed.
  Synthetic: release URLs, HTTPS refs, and scheme-less refs become located declarations;
  coordinated URLs retain the same selected release. Local paths and comments stay excluded.
- [ ] <a id="u5"></a>**U5 — Digest/channel/commit image updates.** The 51 bindings include 38 that
  cannot currently be checked: 24 digest pins, 8 mutable tags, 4 tagless references,
  `sha-4fd4faa`, and `arch-kde`. Examples include hermes tag+digest, zfsreplication
  digest-only, ente/Valkey digests embedded in `image.tag`, `latest`, `main`, `busybox`,
  and a tagless registry with a port. Synthetic: each remains visible and unchanged;
  any future digest refresh or channel policy preserves the pin and never invents a tag.
- [ ] <a id="u6"></a>**U6 — Stable release/channel selection** ([#15](https://github.com/mathiasringhof/fluxrepo-update/issues/15)).
  Three Jellyfin image declarations can be offered unstable timestamp builds.
  Synthetic registry: current `10.11.8`, candidates `10.11.8`, `12.1`, `2026092811`;
  never propose the timestamp as a stable upgrade. Keep valid calendar-to-calendar
  updates. Also establish expected chart handling for cert-manager `v1.21.1`,
  rke2-multus `v4.3.017`, and unpoller `2.11.2-Chart6`; a prerelease/chart suffix must
  follow chart rules, not image-variant rules (T3).
- [ ] <a id="u7"></a>**U7 — Images inside post-renderer patch strings.**
  `apps/apptesting/immich/release-patch.yaml` embeds `busybox:1.36` in
  `spec.postRenderers[0].kustomize.patches[0].patch`, a JSON6902 add of init containers.
  Synthetic: one ordinary image plus this block-string image; discover both with distinct
  locations, initially reporting the patch image unchecked. Future writes must preserve
  the outer scalar style and validate both inner patch and outer manifest.
- [ ] <a id="u8"></a>**U8 — Image-valued environment variables.**
  `apps/base/zfsreplication/deployment.yaml` repeats the controller digest in
  `env[RELEASE_IMAGE].value`. `apps/production/{openssh,webtop}/deployment.yaml` have
  `DOCKER_MODS` values with three and one image references respectively.
  Synthetic: repeated digest plus pipe-separated refs, with arbitrary environment text
  as a negative control. Discover through an explicit schema, retain each token's location,
  and keep coordinated references consistent before permitting edits.
- [ ] <a id="u9"></a>**U9 — VM image sources.** Eight `DataVolume.spec.source.http.url` declarations
  occur in `kubevirt/production/vms/{k3s-apptesting,haos,wgvm}/rootdisk-dv.yaml`,
  `kubevirt/production/lab/vms/resources.yaml` (three documents), and
  `kubevirt/testing/{wgvm-ubuntu-import-dv,vm-102-import-dv}.yaml`.
  Five use Ubuntu `noble/current`; three are private imported disks with no known
  upstream version. ISO paths occur in `kubevirt/{apptesting,testing}/ubnt3-autoinstall-test.yaml`
  (`24.04.4`) and `kubevirt/testing/debian-vm/debian.yaml` (`13.1.0`). Synthetic:
  explicit ISO version, mutable cloud image URL, opaque private disk URL, and an ordinary
  writable disk path. Report provenance/unknown availability; never interpret VM IDs as versions
  or rewrite a disk path without a provisioned replacement.
- [ ] <a id="u10"></a>**U10 — Dependencies inside executable content.**
  `kubevirt/production/lab/runner/run.py` downloads K3s `v1.35.8+k3s1` and its checksum
  file and builds Pods with `busybox:1.37.0`; `directrouting.py` builds a digest-pinned
  Python Pod. ConfigMap generators package these source files; version assertions also
  occur in runner/gate scripts. Synthetic: generated ConfigMap source plus embedded code,
  coupled binary/checksum URLs, and runtime-generated Pod images. Establish an explicit
  extraction/provenance boundary; do not execute scripts to discover dependencies.
  Unpinned cloud-init/install scripts and package-manager commands also have unknown
  runtime dependency versions, rather than an editable version declaration.
- [ ] <a id="u11"></a>**U11 — Repository tooling dependencies.** `tests/requirements.txt` pins
  `PyYAML==6.0.3` and `cel-python==0.5.0`; `scripts/lab/guest.py` pins virtctl by checksum
  without a release identity. Synthetic: two requirement pins plus an opaque checksum;
  report these as an explicit scope extension or delegate them to a package/tool updater.
  Do not infer a release from a checksum.
- [ ] <a id="u12"></a>**U12 — Whole bundles and Flux bootstrap upgrades.**
  `apps/apptesting/kubevirt-manager/bundled-v1.5.3.yaml` contains image `1.5.4` while
  filename and version labels/selectors still identify `1.5.3`. Its image update is not
  a bundle/CRD refresh. Six generated `clusters/*/flux-system/gotk-*` files contain
  twelve controller images and Flux versions `2.7.5`, `2.8.5`, `2.8.6`.
  Synthetic: bundle metadata and image disagree; generated files remain byte-identical.
  Track upstream bundle and bootstrap refresh separately; generated files stay excluded
  from automatic edits under the repository rules.
- [ ] <a id="u13"></a>**U13 — Completeness and failed resolution** ([#18](https://github.com/mathiasringhof/fluxrepo-update/issues/18)).
  Recognized counts already distinguish checked and unchecked declarations, but misses
  above are absent from both. Synthetic: fully current, all requests fail, mixed success,
  unsupported-only, and an undiscovered category. Define a completeness signal without
  treating exit zero or zero planned updates as proof of repository-wide currency.
- [ ] <a id="u14"></a>**U14 — Grafana dashboard revisions.**
  `apps/{production,apptesting}/kube-prometheus-stack/kube-prometheus-stack-values.yaml`
  pins `spec.values.grafana.dashboards.default.smartctl_exporter` with `gnetId: 22604`
  and `revision: 2`. Production also has five unpoller dashboard URLs ending in
  `/revisions/latest/download`. Synthetic: a dashboard ID/revision pair, a mutable
  dashboard URL, and unrelated numeric/revision fields. Discover the explicit revision
  and report unknown availability for the channel; preserve dashboard identity and do
  not interpret `gnetId` itself as a version.
- [ ] <a id="u15"></a>**U15 — Applying valid YAML with indentless lists.** The synthetic exercise
  exposed a writer failure after discovery and planning: valid sibling block sequences
  can produce a false missing-path/changed-target error during apply. This is an
  additional correctness gap, not another dependency category in the kubeflux inventory.
  [Workload lists](../coverage/kubeflux/cases/yaml-indentless-workload-lists/case.json)
  and [Helm values lists](../coverage/kubeflux/cases/yaml-indentless-helm-values-lists/case.json)
  require all selected images to update while preserving YAML style and unrelated fields.
  Both remain TODO; their expected output retains the desired edits.

## Implemented synthetic cases for existing behavior

T1–T4 are implemented in the separate corpus through the public CLI, with local HTTP
registry/index responses for version selection and byte-for-byte apply checks. Checked
boxes mean the examples exist, not that every desired capability passes; the
[status table](../coverage/kubeflux/STATUS.md) records each outcome.

- [x] <a id="t1"></a>**T1 — Helm values lists.** [Executable case](../coverage/kubeflux/cases/helm-values-lists/case.json). Jellyfin/nginx-style `initContainers[]` and
  `extraContainers[]`: plan and apply versioned scalars/mappings, preserve adjacent
  tagless/mutable images, and verify the exact indexed field and unchanged neighbors.
- [x] <a id="t2"></a>**T2 — Exact conservative image forms.** [Cases and outcomes](../coverage/kubeflux/STATUS.md). Cover digest-only, tag+digest,
  digest inside a mapping's `tag`, tag-only digest, `main`, `arch-kde`, `sha-4fd4faa`,
  and tagless host:port references. Assert one declaration per field, appropriate
  unchecked reason, and no writes; avoid network-dependent skips masking classification.
- [x] <a id="t3"></a>**T3 — Real version-selection workflows.** [Date example](../coverage/kubeflux/cases/version-date/case.json), [OpenSSH example](../coverage/kubeflux/cases/version-linuxserver-openssh/case.json). Exercise date-only `20260830`,
  LinuxServer `version-10.3_p1-r0`, and the chart spellings from U6 through a local
  registry/index and apply. Verify selected versions, no downgrade, exact tag spelling,
  and preservation of suffixes. Static resolvers alone cannot establish this behavior.
- [x] <a id="t4"></a>**T4 — Negative controls.** [Executable case](../coverage/kubeflux/cases/negative-controls/case.json). SOPS format `version`, Kubernetes API versions,
  network-data `version: 2`, arbitrary numbers, commented-out pins, and ordinary disk
  paths must not turn into dependency updates. Include configuration rollout revisions,
  CNI/syslog/NFS schema or protocol versions, and a real adjacent image that still updates.

For every added category: a minimal anonymous fixture, exact discovery location, plan
or explicit unchecked outcome, and apply/preservation assertion. Develop each case
red-first before its implementation. Do not copy full application manifests, encrypted
secrets, live registry responses, or the entire kubeflux checkout into tests.

No `chartRef`, `OCIRepository`, Kustomize `images` transformer, or CI workflow was found
in this snapshot. They remain broader product coverage candidates, not observed kubeflux
gaps. Secret contents, chart defaults, rendered remote resources, and downloaded scripts
were not expanded, so this audit cannot claim transitive dependency coverage.

To refresh: record both commits and working-tree status, run `inventory --json` against
kubeflux, independently inspect YAML and non-YAML version-bearing structures, then map
each new shape to a tested category or an unchecked TODO. Re-run the CI checks; never
use the scanner's own output as the sole evidence that discovery is complete.
