use super::HumanOutput;
use super::interactive_approval::InteractiveApproval;
use crate::update_run::UpdatePlan;
use crate::update_run::UpdateSelectionIdentity;
use crate::update_run::{PlannedUpdate, SkippedUpdate};
use std::cell::RefCell;
use std::io::Cursor;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::rc::Rc;

#[test]
fn github_check_counts_count_declarations_instead_of_approval_groups() {
    use crate::github::GitHubReleaseResolver;
    use crate::resolvers::{RegistryImageResolver, RepositoryChartResolver};
    use crate::test_support::{ResponseSpec, TestHttpServer, write_file};
    use crate::update_run::UpdateRun;
    use serde_json::{Value, json};

    for (status, body, completeness, code, unchanged, available, failed, reason) in [
        (
            200,
            json!([{"tag_name":"v1.0.0", "draft":false, "prerelease":false, "assets":[]}])
                .to_string(),
            "complete",
            0,
            2,
            0,
            0,
            "",
        ),
        (
            200,
            json!([{"tag_name":"v2.0.0", "draft":false, "prerelease":false, "assets":[]}])
                .to_string(),
            "complete",
            10,
            0,
            2,
            0,
            "",
        ),
        (
            503,
            "unavailable".into(),
            "failed",
            2,
            0,
            0,
            2,
            "github_request_failed",
        ),
        (
            200,
            "{}".into(),
            "failed",
            2,
            0,
            0,
            2,
            "github_release_metadata_invalid",
        ),
        (
            200,
            "[]".into(),
            "failed",
            2,
            0,
            0,
            2,
            "github_release_version_unavailable",
        ),
    ] {
        let repo = tempfile::tempdir().expect("repo");
        let original = "resources:\n- github.com/example/operator/one?ref=v1.0.0\n---\nresources:\n- https://github.com/example/operator/two?ref=v1.0.0\n";
        let path = repo.path().join("kustomization.yaml");
        write_file(&path, original);
        let server = TestHttpServer::new(vec![ResponseSpec::new(status, body)]);
        let remote = GitHubReleaseResolver::builder()
            .api_base_url(server.base_url.clone())
            .build();
        let charts = RepositoryChartResolver::default();
        let images = RegistryImageResolver::default();
        let run = UpdateRun::new(&charts, &images).with_remote_resource_resolver(&remote);
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let actual_code = super::update_helm_command(
            super::UpdateHelmArgs {
                repo_root: repo.path().to_path_buf(),
                json_output: true,
                write: false,
                non_interactive: true,
                apply_ids: Vec::new(),
            },
            InteractiveApproval::plain(Cursor::new(b"")),
            &mut stdout,
            &mut stderr,
            &run,
            HumanOutput::plain(),
        )
        .expect("run CLI command");

        assert_eq!(actual_code, code);
        assert!(stderr.is_empty());
        let report: Value = serde_json::from_slice(&stdout).expect("report");
        assert_eq!(report["check_completeness"], completeness);
        assert_eq!(report["summary"]["discovered_count"], 2);
        assert_eq!(report["summary"]["checked_count"], unchanged + available);
        assert_eq!(report["summary"]["checked_unchanged_count"], unchanged);
        assert_eq!(report["summary"]["update_available_count"], available);
        assert_eq!(report["summary"]["unsupported_count"], 0);
        assert_eq!(report["summary"]["failed_count"], failed);
        assert_eq!(report["summary"]["unchecked_count"], failed);
        assert_eq!(report["summary"]["skipped_count"], failed);
        assert_eq!(
            report["summary"]["planned_count"],
            usize::from(available > 0)
        );
        if failed > 0 {
            let skipped = report["skipped"].as_array().expect("skips");
            assert_eq!(skipped.len(), 2);
            assert!(skipped.iter().all(|skip| skip["reason_code"] == reason));
        }
        assert_eq!(
            std::fs::read_to_string(path).expect("unmodified resources"),
            original
        );
        assert_eq!(server.finish().len(), 1);
    }
}

#[test]
fn remote_resource_approval_shows_every_member_and_approves_one_group() {
    struct Remote;
    impl crate::github::RemoteResourceVersionResolver for Remote {
        fn resolve(&self, _: &str, _: &str, _: &str, _: &[String]) -> anyhow::Result<String> {
            Ok("v2.0.0".into())
        }
    }
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join("kustomization.yaml"), "resources:\n- github.com/example/operator/one?ref=v1.0.0\n---\nresources:\n- https://github.com/example/operator/two?ref=v1.0.0\n").unwrap();
    let charts = crate::resolvers::StaticVersionResolver::new(std::collections::HashMap::new());
    let images =
        crate::resolvers::StaticImageVersionResolver::new(std::collections::HashMap::new());
    let outcome = crate::update_run::UpdateRun::new(&charts, &images)
        .with_remote_resource_resolver(&Remote)
        .execute(
            &crate::scanner::scan_repo(temp.path()).unwrap(),
            crate::update_run::UpdateRunMode::PlanOnly,
            &mut |_| unreachable!(),
            None,
        )
        .unwrap();
    let mut output = Vec::new();
    let approved = InteractiveApproval::plain(Cursor::new(b"y"))
        .review(
            &outcome.plan().review(),
            &mut output,
            temp.path(),
            HumanOutput::plain(),
        )
        .unwrap();
    assert_eq!(approved.len(), 1);
    let output = String::from_utf8(output).unwrap();
    assert_eq!(output.matches("[y/N]").count(), 1);
    for detail in [
        "document 1, resources[0]",
        "document 2, resources[0]",
        "github.com/example/operator/one?ref=v1.0.0 -> github.com/example/operator/one?ref=v2.0.0",
        "https://github.com/example/operator/two?ref=v1.0.0 -> https://github.com/example/operator/two?ref=v2.0.0",
    ] {
        assert!(output.contains(detail), "missing {detail}: {output}");
    }
}

#[test]
fn interactive_json_disables_terminal_color_and_progress() {
    let charts = crate::resolvers::StaticVersionResolver::new(std::collections::HashMap::new());
    let images =
        crate::resolvers::StaticImageVersionResolver::new(std::collections::HashMap::from([(
            "example/app:1.0.0".to_string(),
            "example/app:2.0.0".to_string(),
        )]));
    let temp = tempfile::tempdir().expect("temp dir");
    std::fs::write(
        temp.path().join("pod.yaml"),
        "kind: Pod\nmetadata: {name: demo}\nspec: {containers: [{image: example/app:1.0.0}]}\n",
    )
    .expect("write pod");
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let code = super::run_with_args_and_output(
        [
            "fluxrepo-update",
            "update-helm",
            temp.path().to_str().expect("repo path"),
            "--json",
        ],
        InteractiveApproval::plain(Cursor::new(b"n")),
        &mut stdout,
        &mut stderr,
        &charts,
        &images,
        HumanOutput {
            color: true,
            progress: true,
        },
    )
    .expect("run interactive JSON command");

    assert_eq!(code, 0);
    let report: serde_json::Value = serde_json::from_slice(&stdout).expect("JSON report");
    assert_eq!(report["mode"], "apply");
    let stderr = String::from_utf8(stderr).expect("UTF-8 stderr");
    assert!(stderr.contains("[y/N]"));
    assert!(!stderr.contains('\u{1b}'));
    assert!(!stderr.contains("Resolving"));
}

#[test]
fn plain_approval_filters_planned_updates_and_preserves_skips() {
    let report = report_with_updates(&[
        "first.yaml",
        "second.yaml",
        "third.yaml",
        "fourth.yaml",
        "fifth.yaml",
    ]);
    let mut output = Vec::new();
    let approval = InteractiveApproval::plain(Cursor::new(b"yYNx"));

    let approved = approval
        .review(
            &report.review(),
            &mut output,
            Path::new("/repo"),
            HumanOutput::plain(),
        )
        .expect("review updates");

    assert_eq!(approved.len(), 2);
    assert_eq!(approved[0], *report.review()[0].identity());
    assert_eq!(approved[1], *report.review()[1].identity());
    assert_eq!(report.skipped().len(), 1);
    let output = String::from_utf8(output).expect("utf-8");
    assert_eq!(output.matches("[y/N] y\n").count(), 2);
    assert_eq!(output.matches("[y/N] n\n").count(), 3);
    let prompt_positions = [
        "first.yaml",
        "second.yaml",
        "third.yaml",
        "fourth.yaml",
        "fifth.yaml",
    ]
    .map(|path| output.find(path).expect("prompt path"));
    assert!(prompt_positions.windows(2).all(|pair| pair[0] < pair[1]));
}

#[test]
fn plain_approval_stops_on_ctrl_c_with_an_interrupted_error() {
    let report = report_with_updates(&["first.yaml", "second.yaml"]);
    let mut output = Vec::new();
    let approval = InteractiveApproval::plain(Cursor::new([0x03, b'y']));

    let error = approval
        .review(
            &report.review(),
            &mut output,
            Path::new("/repo"),
            HumanOutput::plain(),
        )
        .expect_err("Ctrl-C should interrupt");

    assert_eq!(
        error
            .downcast_ref::<std::io::Error>()
            .map(std::io::Error::kind),
        Some(std::io::ErrorKind::Interrupted)
    );
    assert_eq!(
        String::from_utf8(output)
            .expect("utf-8")
            .matches("[y/N]")
            .count(),
        1
    );
}

#[test]
fn plain_approval_returns_no_selection_after_prompt_failure() {
    assert_plain_approval_failure(ApprovalFault::Prompt);
}

#[test]
fn plain_approval_returns_no_selection_after_flush_failure() {
    assert_plain_approval_failure(ApprovalFault::Flush);
}

#[test]
fn plain_approval_returns_no_selection_after_read_failure() {
    assert_plain_approval_failure(ApprovalFault::Read);
}

#[test]
fn plain_approval_returns_no_selection_after_echo_failure() {
    assert_plain_approval_failure(ApprovalFault::Echo);
}

#[test]
fn plain_approval_flushes_each_prompt_before_reading() {
    let plan = report_with_updates(&["first.yaml", "second.yaml"]);
    let io = Rc::new(RefCell::new(ApprovalIo::default()));
    let approved = InteractiveApproval::plain(ObservedInput(Rc::clone(&io)))
        .review(
            &plan.review(),
            &mut ObservedOutput(Rc::clone(&io)),
            Path::new("/repo"),
            HumanOutput::plain(),
        )
        .expect("review updates");

    assert_eq!(
        approved,
        plan.review()
            .iter()
            .map(|update| update.identity().clone())
            .collect::<Vec<_>>()
    );
    let io = io.borrow();
    assert_eq!(io.reads.len(), 2);
    for (index, path) in ["first.yaml", "second.yaml"].into_iter().enumerate() {
        let visible = String::from_utf8_lossy(&io.reads[index]);
        assert!(
            visible.contains(path),
            "prompt must be visible before reading"
        );
        assert!(visible.ends_with("[y/N] "));
        assert_eq!(visible.matches("[y/N]").count(), index + 1);
    }
    assert!(!String::from_utf8_lossy(&io.reads[0]).contains("second.yaml"));
}

fn assert_plain_approval_failure(fault: ApprovalFault) {
    for earlier_approvals in [0, 1] {
        let io = Rc::new(RefCell::new(ApprovalIo {
            fault: Some((fault, earlier_approvals)),
            ..ApprovalIo::default()
        }));
        let error = InteractiveApproval::plain(ObservedInput(Rc::clone(&io)))
            .review(
                &report_with_updates(&["first.yaml", "second.yaml", "third.yaml"]).review(),
                &mut ObservedOutput(Rc::clone(&io)),
                Path::new("/repo"),
                HumanOutput::plain(),
            )
            .expect_err("an I/O failure must discard the entire selection");

        assert_io_error_kind(&error, std::io::ErrorKind::Other);
        assert!(error.to_string().contains(&format!("{fault:?} failed")));
        let io = io.borrow();
        assert_eq!(io.failure, Some(fault), "reach the intended failure");
        assert_eq!(io.echoed, earlier_approvals);
        let output = String::from_utf8_lossy(&io.written);
        assert_eq!(output.matches("[y/N] y\n").count(), earlier_approvals);
        assert!(!output.contains("third.yaml"));
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ApprovalFault {
    Prompt,
    Flush,
    Read,
    Echo,
}

#[derive(Default)]
struct ApprovalIo {
    written: Vec<u8>,
    flushed_len: usize,
    // Only bytes made visible by flush are observable to a reader.
    reads: Vec<Vec<u8>>,
    echoed: usize,
    echoing: bool,
    fault: Option<(ApprovalFault, usize)>,
    failure: Option<ApprovalFault>,
}

impl ApprovalIo {
    fn fail_if_requested(&mut self, operation: ApprovalFault) -> std::io::Result<()> {
        if self.fault == Some((operation, self.echoed)) {
            self.failure = Some(operation);
            return Err(std::io::Error::other(format!("{operation:?} failed")));
        }
        Ok(())
    }
}

struct ObservedInput(Rc<RefCell<ApprovalIo>>);

impl std::io::Read for ObservedInput {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        let mut io = self.0.borrow_mut();
        let visible = io.written[..io.flushed_len].to_vec();
        io.reads.push(visible);
        io.fail_if_requested(ApprovalFault::Read)?;
        buffer[0] = b'y';
        Ok(1)
    }
}

struct ObservedOutput(Rc<RefCell<ApprovalIo>>);

impl std::io::Write for ObservedOutput {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let mut io = self.0.borrow_mut();
        let operation = if io.echoing {
            ApprovalFault::Echo
        } else {
            ApprovalFault::Prompt
        };
        io.fail_if_requested(operation)?;
        io.written.extend_from_slice(buffer);
        if io.echoing && buffer.contains(&b'\n') {
            io.echoed += 1;
            io.echoing = false;
        }
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        let mut io = self.0.borrow_mut();
        io.fail_if_requested(ApprovalFault::Flush)?;
        io.flushed_len = io.written.len();
        io.echoing = true;
        Ok(())
    }
}

fn assert_io_error_kind(error: &anyhow::Error, kind: std::io::ErrorKind) {
    assert_eq!(
        error
            .chain()
            .find_map(|cause| cause.downcast_ref::<std::io::Error>())
            .map(std::io::Error::kind),
        Some(kind),
        "{error:#}"
    );
}

#[cfg(unix)]
#[test]
fn terminal_approval_reads_immediately_and_restores_the_same_pty() {
    let (result, output, original, restored) = review_through_pty(Some(b'y'));
    let approved = result.expect("review through PTY");

    assert_eq!(approved.len(), 1);
    assert!(output.ends_with(b"[y/N] y\r\n"));
    assert_termios_restored(&original, &restored);
}

#[cfg(unix)]
#[test]
fn terminal_approval_reports_ctrl_c_and_restores_before_returning() {
    let (result, output, original, restored) = review_through_pty(Some(0x03));
    let error = result.expect_err("Ctrl-C should interrupt");

    assert_eq!(
        error
            .downcast_ref::<std::io::Error>()
            .map(std::io::Error::kind),
        Some(std::io::ErrorKind::Interrupted)
    );
    assert_eq!(
        String::from_utf8(output)
            .expect("utf-8")
            .matches("[y/N]")
            .count(),
        1
    );
    assert_termios_restored(&original, &restored);
}

#[cfg(unix)]
#[test]
fn terminal_approval_restores_the_pty_after_rejection() {
    for decision in [b'n', b'x'] {
        let (result, output, original, restored) = review_through_pty(Some(decision));

        assert!(result.expect("reject update").is_empty());
        assert!(output.ends_with(b"[y/N] n\r\n"));
        assert_termios_restored(&original, &restored);
    }
}

#[cfg(unix)]
#[test]
fn terminal_approval_restores_the_pty_after_end_of_input() {
    let (result, output, original, restored) = review_through_pty(None);

    assert!(result.expect("reject on a zero-byte read").is_empty());
    assert!(output.ends_with(b"[y/N] n\r\n"));
    assert_termios_restored(&original, &restored);
}

#[cfg(unix)]
#[test]
fn terminal_approval_restores_the_pty_after_flush_failure() {
    assert_terminal_output_failure(ApprovalFault::Flush);
}

#[cfg(unix)]
#[test]
fn terminal_approval_restores_the_pty_after_echo_failure() {
    assert_terminal_output_failure(ApprovalFault::Echo);
}

#[cfg(unix)]
fn assert_terminal_output_failure(fault: ApprovalFault) {
    use rustix::termios::tcgetattr;

    for earlier_approvals in [0, 1] {
        let (mut master, slave) = open_pty(true);
        let observer = slave.try_clone().expect("observe PTY");
        let original = tcgetattr(&observer).expect("original terminal settings");
        let io = Rc::new(RefCell::new(ApprovalIo {
            fault: Some((fault, earlier_approvals)),
            ..ApprovalIo::default()
        }));
        let mut output = OnFlush {
            output: ObservedOutput(Rc::clone(&io)),
            on_flush: || {
                bound_pty_read(&observer);
                master.write_all(b"y")
            },
        };

        let error = InteractiveApproval::classified(slave)
            .review(
                &report_with_updates(&["first.yaml", "second.yaml", "third.yaml"]).review(),
                &mut output,
                Path::new("/repo"),
                HumanOutput::plain(),
            )
            .expect_err("output failure must discard the entire selection");

        assert_io_error_kind(&error, std::io::ErrorKind::Other);
        assert!(error.to_string().contains(&format!("{fault:?} failed")));
        assert_eq!(io.borrow().failure, Some(fault));
        assert_eq!(io.borrow().echoed, earlier_approvals);
        let io = io.borrow();
        let output = String::from_utf8_lossy(&io.written);
        assert_eq!(output.matches("[y/N] y\r\n").count(), earlier_approvals);
        assert!(!output.contains("third.yaml"));
        assert_termios_restored(&original, &tcgetattr(&observer).expect("restored settings"));
    }
}

#[cfg(unix)]
#[test]
fn terminal_approval_restores_the_pty_after_read_failure() {
    use rustix::termios::tcgetattr;

    // A write-only slave is still a terminal, but read(2) fails with EBADF.
    let (_master, slave) = open_pty(false);
    let observer = slave.try_clone().expect("observe PTY");
    let original = tcgetattr(&observer).expect("original terminal settings");
    let mut output = OnFlush {
        output: Vec::new(),
        on_flush: || {
            bound_pty_read(&observer);
            Ok(())
        },
    };

    let error = InteractiveApproval::classified(slave)
        .review(
            &report_with_updates(&["first.yaml", "second.yaml"]).review(),
            &mut output,
            Path::new("/repo"),
            HumanOutput::plain(),
        )
        .expect_err("reading a write-only PTY must fail");

    assert_eq!(
        error
            .chain()
            .find_map(|cause| cause.downcast_ref::<std::io::Error>())
            .and_then(std::io::Error::raw_os_error),
        Some(rustix::io::Errno::BADF.raw_os_error())
    );
    assert!(!String::from_utf8_lossy(&output.output).contains("second.yaml"));
    assert_termios_restored(&original, &tcgetattr(&observer).expect("restored settings"));
}

#[cfg(unix)]
#[test]
fn terminal_approval_returns_restoration_failure_without_a_selection() {
    if !in_pty_child("terminal_approval_returns_restoration_failure_without_a_selection") {
        return;
    }
    let (result, output) = review_with_replaced_terminal(b'y');
    let error = result.expect_err("restoration failure must discard both approvals");

    assert_eq!(
        error.downcast_ref::<rustix::io::Errno>(),
        Some(&rustix::io::Errno::NOTTY)
    );
    assert_eq!(
        String::from_utf8_lossy(&output)
            .matches("[y/N] y\r\n")
            .count(),
        2
    );
}

#[cfg(unix)]
#[test]
fn terminal_approval_keeps_ctrl_c_primary_when_restoration_fails() {
    if !in_pty_child("terminal_approval_keeps_ctrl_c_primary_when_restoration_fails") {
        return;
    }
    let (result, output) = review_with_replaced_terminal(0x03);
    let error = result.expect_err("Ctrl-C and failed restoration must discard the first approval");

    assert_io_error_kind(&error, std::io::ErrorKind::Interrupted);
    assert_eq!(
        error
            .root_cause()
            .downcast_ref::<std::io::Error>()
            .map(std::io::Error::kind),
        Some(std::io::ErrorKind::Interrupted)
    );
    let diagnostic = error.to_string();
    assert!(diagnostic.contains("prompt interrupted"));
    assert!(diagnostic.contains("terminal restoration failed"));
    assert!(diagnostic.contains(&rustix::io::Errno::NOTTY.to_string()));
    assert_eq!(
        String::from_utf8_lossy(&output)
            .matches("[y/N] y\r\n")
            .count(),
        1
    );
}

#[cfg(unix)]
fn review_with_replaced_terminal(
    decision: u8,
) -> (anyhow::Result<Vec<UpdateSelectionIdentity>>, Vec<u8>) {
    use std::io::{BufRead as _, IsTerminal as _};

    let (mut master, slave) = open_pty(true);
    rustix::stdio::dup2_stdin(&slave).expect("install child PTY as stdin");
    let stdin = std::io::stdin();
    assert!(stdin.is_terminal());
    let mut decisions = [b'y', decision].into_iter();
    let mut output = OnFlush {
        output: Vec::new(),
        on_flush: || {
            bound_pty_read(&stdin);
            let byte = decisions.next().expect("one decision per prompt");
            master.write_all(&[byte])?;
            if decisions.len() == 0 {
                // Keep the real PTY byte buffered in this same Stdin handle, then
                // replace fd 0. read() can complete, but tcsetattr(0) must fail
                // with ENOTTY. Unlike closing the master, this works on macOS too.
                assert_eq!(stdin.lock().fill_buf()?, &[byte]);
                let non_terminal = tempfile::tempfile()?;
                rustix::stdio::dup2_stdin(&non_terminal)?;
                assert!(!stdin.is_terminal());
            }
            Ok(())
        },
    };
    let result = InteractiveApproval::classified(std::io::stdin()).review(
        &report_with_updates(&["first.yaml", "second.yaml"]).review(),
        &mut output,
        Path::new("/repo"),
        HumanOutput::plain(),
    );
    (result, output.output)
}

#[cfg(unix)]
fn in_pty_child(test_name: &str) -> bool {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    const CHILD: &str = "FLUXREPO_APPROVAL_PTY_CHILD";
    if std::env::var(CHILD).as_deref() == Ok(test_name) {
        return true;
    }
    // Descriptor replacement must never affect the parallel test runner's stdin.
    let mut child = Command::new(std::env::current_exe().expect("test executable"))
        .args([
            "--exact",
            &format!("cli::tests::{test_name}"),
            "--nocapture",
        ])
        .env(CHILD, test_name)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start isolated PTY test");
    let deadline = Instant::now() + Duration::from_secs(10);
    while child.try_wait().expect("check PTY child").is_none() {
        if Instant::now() >= deadline {
            child.kill().expect("stop stalled PTY child");
            let output = child.wait_with_output().expect("reap stalled child");
            panic!("PTY child timed out: {output:?}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().expect("collect PTY child");
    assert!(
        output.status.success(),
        "PTY child failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    // A filter that matches nothing would otherwise pass without running the test.
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("1 passed"),
        "PTY child did not run {test_name}"
    );
    false
}

#[cfg(unix)]
#[test]
fn terminal_approval_restores_the_pty_after_output_failure() {
    assert_terminal_output_failure(ApprovalFault::Prompt);
}

#[cfg(unix)]
fn review_through_pty(
    decision: Option<u8>,
) -> (
    anyhow::Result<Vec<UpdateSelectionIdentity>>,
    Vec<u8>,
    rustix::termios::Termios,
    rustix::termios::Termios,
) {
    use rustix::termios::tcgetattr;

    let (mut master, slave) = open_pty(true);
    let observer = slave.try_clone().expect("clone PTY slave");
    let original = tcgetattr(&observer).expect("original terminal settings");
    let mut output = OnFlush {
        output: Vec::new(),
        on_flush: || {
            bound_pty_read(&observer);
            if let Some(byte) = decision {
                master.write_all(&[byte])?;
            }
            Ok(())
        },
    };
    let result = InteractiveApproval::classified(slave).review(
        &report_with_updates(&["first.yaml"]).review(),
        &mut output,
        Path::new("/repo"),
        HumanOutput::plain(),
    );
    let restored = tcgetattr(&observer).expect("restored settings");
    (result, output.output, original, restored)
}

#[cfg(unix)]
fn open_pty(readable: bool) -> (std::fs::File, std::fs::File) {
    use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};

    let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).expect("open PTY master");
    grantpt(&master).expect("grant PTY");
    unlockpt(&master).expect("unlock PTY");
    let slave_path = ptsname(&master, Vec::new()).expect("PTY slave path");
    let slave = std::fs::OpenOptions::new()
        .read(readable)
        .write(true)
        .open(slave_path.to_string_lossy().as_ref())
        .expect("open PTY slave");
    (master.into(), slave)
}

#[cfg(unix)]
struct OnFlush<W, F> {
    output: W,
    on_flush: F,
}

#[cfg(unix)]
impl<W: std::io::Write, F: FnMut() -> std::io::Result<()>> std::io::Write for OnFlush<W, F> {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.output.write(buffer)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.output.flush()?;
        (self.on_flush)()
    }
}

#[cfg(unix)]
fn bound_pty_read(fd: &impl std::os::fd::AsFd) {
    use rustix::termios::{LocalModes, OptionalActions, SpecialCodeIndex, tcgetattr, tcsetattr};

    let mut raw = tcgetattr(fd).expect("active terminal settings");
    assert!(
        !raw.local_modes
            .intersects(LocalModes::ICANON | LocalModes::ECHO | LocalModes::ISIG)
    );
    // A timed noncanonical read returns zero bytes without hanging up the PTY.
    // This exercises end-of-input while leaving the terminal available for
    // restoration, and bounds waits if a decision is unexpectedly not delivered.
    raw.special_codes[SpecialCodeIndex::VMIN] = 0;
    raw.special_codes[SpecialCodeIndex::VTIME] = 1;
    tcsetattr(fd, OptionalActions::Now, &raw).expect("bound PTY read");
}

#[cfg(unix)]
fn assert_termios_restored(
    original: &rustix::termios::Termios,
    restored: &rustix::termios::Termios,
) {
    use rustix::termios::{LocalModes, SpecialCodeIndex};

    assert_eq!(restored.input_modes, original.input_modes);
    assert_eq!(restored.output_modes, original.output_modes);
    assert_eq!(restored.control_modes, original.control_modes);
    // macOS can set PENDIN when canonical processing is re-enabled.
    assert_eq!(
        restored.local_modes - LocalModes::PENDIN,
        original.local_modes - LocalModes::PENDIN
    );
    assert_eq!(restored.input_speed(), original.input_speed());
    assert_eq!(restored.output_speed(), original.output_speed());
    for code in [
        SpecialCodeIndex::VMIN,
        SpecialCodeIndex::VTIME,
        SpecialCodeIndex::VINTR,
        SpecialCodeIndex::VQUIT,
        SpecialCodeIndex::VERASE,
        SpecialCodeIndex::VKILL,
        SpecialCodeIndex::VEOF,
        SpecialCodeIndex::VSTART,
        SpecialCodeIndex::VSTOP,
        SpecialCodeIndex::VSUSP,
        SpecialCodeIndex::VEOL,
        SpecialCodeIndex::VREPRINT,
        SpecialCodeIndex::VDISCARD,
        SpecialCodeIndex::VWERASE,
        SpecialCodeIndex::VLNEXT,
        SpecialCodeIndex::VEOL2,
        #[cfg(target_os = "linux")]
        SpecialCodeIndex::VSWTC,
        #[cfg(target_os = "macos")]
        SpecialCodeIndex::VDSUSP,
        #[cfg(target_os = "macos")]
        SpecialCodeIndex::VSTATUS,
    ] {
        assert_eq!(restored.special_codes[code], original.special_codes[code]);
    }
    #[cfg(target_os = "linux")]
    assert_eq!(restored.line_discipline, original.line_discipline);
}

#[test]
fn interactive_approval_interrupt_applies_neither_manifest() {
    use crate::resolvers::{StaticImageVersionResolver, StaticVersionResolver};
    use std::collections::HashMap;

    let repo = tempfile::tempdir().expect("repo");
    let originals = ["first", "second"].map(|name| {
        let path = repo.path().join(format!("{name}.yaml"));
        let bytes = format!(
            "# Preserve this comment and formatting\nkind: Pod\nmetadata: {{name: {name}}}\nspec:\n  containers:\n    - name: app\n      image: 'example/{name}:1.0.0'\n"
        ).into_bytes();
        std::fs::write(&path, &bytes).expect("write manifest");
        (path, bytes)
    });
    let charts = StaticVersionResolver::new(HashMap::new());
    let images = StaticImageVersionResolver::new(
        ["first", "second"]
            .map(|name| {
                (
                    format!("example/{name}:1.0.0"),
                    format!("example/{name}:2.0.0"),
                )
            })
            .into(),
    );
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    // Interactive mode already applies approved updates. --write requires
    // --non-interactive and would reject arguments before reaching review.
    let error = super::run_with_args(
        [
            "fluxrepo-update",
            "update-helm",
            repo.path().to_str().expect("repo path"),
        ],
        Cursor::new([b'y', 0x03]),
        &mut stdout,
        &mut stderr,
        &charts,
        &images,
    )
    .expect_err("Ctrl-C must abort interactive application");

    assert_io_error_kind(&error, std::io::ErrorKind::Interrupted);
    let stderr = String::from_utf8(stderr).expect("UTF-8 stderr");
    assert_eq!(stderr.matches("[y/N]").count(), 2);
    assert_eq!(stderr.matches("[y/N] y\n").count(), 1);
    assert!(
        stderr.find("first.yaml").expect("first prompt")
            < stderr.find("second.yaml").expect("second prompt")
    );
    for (path, bytes) in originals {
        assert_eq!(
            std::fs::read(path).expect("read manifest after interruption"),
            bytes
        );
    }
}

fn report_with_updates(paths: &[&str]) -> UpdatePlan {
    UpdatePlan::new(
        paths
            .iter()
            .map(|path| {
                PlannedUpdate::chart_for_test(
                    Path::new("/repo"),
                    PathBuf::from("/repo").join(path),
                    0,
                    path,
                    ("repo", "chart"),
                    ("1.0.0", "2.0.0"),
                )
            })
            .collect(),
        vec![SkippedUpdate::new(
            Some(PathBuf::from("/repo/skipped.yaml")),
            "unresolved",
        )],
        paths.len(),
    )
}
