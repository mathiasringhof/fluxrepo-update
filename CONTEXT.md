# Flux Repository Updates

This context describes version discovery and updates across a personal Flux repository.

## Language

**Update Target**:
A version-bearing field in a manifest whose artifact and available versions the tool knows how to resolve. A field may be shared by multiple eventual workloads.
_Avoid_: Related resource, Helm update

**Image Reference**:
A container image repository paired with an Explicit Version, whether written as one scalar or as separate repository and tag fields.
_Avoid_: Container version

**Image Binding**:
The association between an Explicit Version and the Image Reference whose available versions determine an update. The association must follow a manifest schema supported by the tool.
_Avoid_: Container relationship, tag guess

**Version Source**:
A public chart repository or container registry from which available versions can be resolved without private credentials.
_Avoid_: Artifact store

**Managed Manifest**:
A user-authored YAML manifest considered for updates. Generated Flux bootstrap manifests and repository-internal files are outside this set.
_Avoid_: YAML file

**Explicit Version**:
A version value written directly in a manifest. Only Explicit Versions are eligible Update Targets; values are not inherited between manifests.
_Avoid_: Effective version, inherited version

**Latest Stable Version**:
The newest non-prerelease, non-mutable version available for an Update Target, including across compatibility boundaries. It must be newer than the Explicit Version; updates never downgrade.
_Avoid_: Current-track version, latest tag

**Update Plan**:
The deterministic proposal describing changes to Update Targets that automation may inspect and apply.
_Avoid_: Preview, dry run

**Unresolved Target**:
A potential Update Target for which no safe, conclusive update can be determined. It remains unchanged while other resolved targets may proceed.
_Avoid_: Failed update
