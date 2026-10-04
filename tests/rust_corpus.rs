//! Passing synthetic scenarios exercise the library with isolated HTTP fixtures.
mod corpus;

macro_rules! cases {
    ($($name:ident),+ $(,)?) => {
        $(
            #[test]
            fn $name() {
                corpus::run(&stringify!($name).replace('_', "-"))
                    .unwrap_or_else(|error| panic!("{error:#}"));
            }
        )+

        #[test]
        fn every_passing_case_is_registered() {
            corpus::registered_cases_match_passing_corpus(&[$(stringify!($name)),+])
                .unwrap_or_else(|error| panic!("{error:#}"));
        }
    };
}

cases! {
    chart_default_image_unknown,
    chart_source_conflict,
    chart_source_equivalence,
    chart_version_leading_zero,
    chart_version_prerelease_suffix,
    chart_version_v_prefix,
    charts_http,
    charts_oci,
    generated_bootstrap_excluded,
    helm_values_lists,
    helm_values_mappings,
    helm_values_scalars,
    image_commit,
    image_commit_digest_mapping,
    image_commit_digest_tag_only,
    image_digest_only,
    image_flavor,
    image_latest,
    image_latest_digest_scalar,
    image_main,
    image_mapping_digest_field,
    image_mapping_tag_digest,
    image_tag_digest,
    image_tag_only_digest,
    image_tagless_port,
    image_template,
    manifest_local_overlays,
    negative_controls,
    remote_git_ref_update,
    remote_release_resource_update,
    report_fully_current,
    report_mixed_resolution,
    report_unsupported_only,
    schemeless_kustomize_resource,
    scope_exclusions,
    secret_valuesfrom_opaque,
    version_date,
    version_linuxserver_numeric,
    version_linuxserver_openssh,
    version_no_downgrade,
    version_variant,
    workload_cronjob,
    workload_daemonset,
    workload_deployment,
    workload_job,
    workload_pod,
    workload_statefulset,
}
