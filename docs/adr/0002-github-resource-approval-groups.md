# Public GitHub resource approval groups

The additional version checking deferred in ADR 0001 now includes explicit public
GitHub release pins in Kustomize resources. Discovery and checking remain separate
from deployment coverage; Kustomize rendering and private credentials remain outside scope.

References sharing a file, case-insensitive GitHub project, and exact current pin form
one Planned Update, including across YAML documents. Select the newest stable release
that contains every required asset name. Preserve each URL by replacing only its pin.
Treat branches, commits, ambiguous URLs, and unsupported queries as unchecked declarations.

One Update Selection Identity approves the whole group. Bind every resource list and
document count in the file so changed or added references invalidate prior approval;
recheck those facts before application. Existing chart and image identity formats remain
unchanged. Coverage counts count individual declarations, while planned/applied counts
count Planned Updates. Resolution failure reports one located skip per group member.

Only public release metadata is fetched. Cache complete metadata or failures per
project within a run, constrain pagination to the API origin, and reject redirects
with the default HTTP client. Missing assets, malformed metadata, and failed requests
leave availability unknown; they do not silently certify a declaration as current.
