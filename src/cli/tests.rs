use super::HumanOutput;
use super::interactive_approval::InteractiveApproval;
use crate::update_run::UpdatePlan;
use crate::update_run::UpdateSelectionIdentity;
use crate::update_run::implementation::{
    PlannedChartUpdate, PlannedUpdate, SkippedUpdate, UpdateReport,
};
use std::io::Cursor;
use std::io::Write as _;
use std::path::{Path, PathBuf};

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

#[cfg(unix)]
#[test]
fn terminal_approval_reads_immediately_and_restores_the_same_pty() {
    let (result, output, original, restored) = review_through_pty(b'y');
    let approved = result.expect("review through PTY");

    assert_eq!(approved.len(), 1);
    assert!(output.ends_with(b"[y/N] y\r\n"));
    assert_eq!(restored.input_modes, original.input_modes);
    assert_eq!(restored.output_modes, original.output_modes);
    assert_eq!(restored.control_modes, original.control_modes);
    assert_eq!(
        restored.local_modes - rustix::termios::LocalModes::PENDIN,
        original.local_modes - rustix::termios::LocalModes::PENDIN
    );
    assert_eq!(restored.input_speed(), original.input_speed());
    assert_eq!(restored.output_speed(), original.output_speed());
}

#[cfg(unix)]
#[test]
fn terminal_approval_reports_ctrl_c_and_restores_before_returning() {
    let (result, output, original, restored) = review_through_pty(0x03);
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
    assert_eq!(restored.input_modes, original.input_modes);
    assert_eq!(restored.output_modes, original.output_modes);
    assert_eq!(restored.control_modes, original.control_modes);
    assert_eq!(
        restored.local_modes - rustix::termios::LocalModes::PENDIN,
        original.local_modes - rustix::termios::LocalModes::PENDIN
    );
}

#[cfg(unix)]
#[test]
fn terminal_approval_restores_the_pty_after_output_failure() {
    use std::fs::OpenOptions;

    use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};
    use rustix::termios::tcgetattr;

    let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).expect("open PTY master");
    grantpt(&master).expect("grant PTY");
    unlockpt(&master).expect("unlock PTY");
    let slave_path = ptsname(&master, Vec::new()).expect("PTY slave path");
    let slave = OpenOptions::new()
        .read(true)
        .write(true)
        .open(slave_path.to_string_lossy().as_ref())
        .expect("open PTY slave");
    let observer = slave.try_clone().expect("clone PTY slave");
    let original = tcgetattr(&observer).expect("original terminal settings");
    let approval = InteractiveApproval::classified(slave);
    let mut output = FailingOutput;

    let error = approval
        .review(
            &report_with_updates(&["first.yaml"]).review(),
            &mut output,
            Path::new("/repo"),
            HumanOutput::plain(),
        )
        .expect_err("prompt output should fail");

    assert_eq!(
        error
            .downcast_ref::<std::io::Error>()
            .map(std::io::Error::kind),
        Some(std::io::ErrorKind::Other)
    );
    let restored = tcgetattr(&observer).expect("restored settings");
    assert_eq!(restored.input_modes, original.input_modes);
    assert_eq!(restored.output_modes, original.output_modes);
    assert_eq!(restored.control_modes, original.control_modes);
    assert_eq!(
        restored.local_modes - rustix::termios::LocalModes::PENDIN,
        original.local_modes - rustix::termios::LocalModes::PENDIN
    );
}

struct FailingOutput;

impl std::io::Write for FailingOutput {
    fn write(&mut self, _buffer: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::other("output failed"))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(unix)]
fn review_through_pty(
    decision: u8,
) -> (
    anyhow::Result<Vec<UpdateSelectionIdentity>>,
    Vec<u8>,
    rustix::termios::Termios,
    rustix::termios::Termios,
) {
    use std::fs::{File, OpenOptions};

    use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};
    use rustix::termios::tcgetattr;

    let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).expect("open PTY master");
    grantpt(&master).expect("grant PTY");
    unlockpt(&master).expect("unlock PTY");
    let slave_path = ptsname(&master, Vec::new()).expect("PTY slave path");
    let slave = OpenOptions::new()
        .read(true)
        .write(true)
        .open(slave_path.to_string_lossy().as_ref())
        .expect("open PTY slave");
    let observer = slave.try_clone().expect("clone PTY slave");
    let original = tcgetattr(&observer).expect("original terminal settings");
    let terminal_observer = observer.try_clone().expect("clone PTY observer");
    let writer = std::thread::spawn(move || {
        while tcgetattr(&terminal_observer)
            .expect("active terminal settings")
            .local_modes
            .contains(rustix::termios::LocalModes::ICANON)
        {
            std::thread::yield_now();
        }
        let mut master = File::from(master);
        master.write_all(&[decision]).expect("send decision byte");
        while !tcgetattr(&terminal_observer)
            .expect("restored terminal settings")
            .local_modes
            .contains(rustix::termios::LocalModes::ICANON)
        {
            std::thread::yield_now();
        }
        master
    });
    let mut output = Vec::new();
    let result = InteractiveApproval::classified(slave).review(
        &report_with_updates(&["first.yaml"]).review(),
        &mut output,
        Path::new("/repo"),
        HumanOutput::plain(),
    );
    // Linux cannot query the slave's settings after the master is closed.
    let _master = writer.join().expect("PTY writer");
    let restored = tcgetattr(&observer).expect("restored settings");
    (result, output, original, restored)
}

fn report_with_updates(paths: &[&str]) -> UpdatePlan {
    let report = UpdateReport {
        checked_count: paths.len(),
        planned: paths
            .iter()
            .map(|path| {
                PlannedUpdate::Chart(PlannedChartUpdate {
                    path: PathBuf::from("/repo").join(path),
                    document_index: 0,
                    target_name: path.to_string(),
                    chart_name: "chart".to_string(),
                    repo_name: "repo".to_string(),
                    current_version: "1.0.0".to_string(),
                    latest_version: "2.0.0".to_string(),
                    manifest_identity: None,
                })
            })
            .collect(),
        skipped: vec![SkippedUpdate::new(
            Some(PathBuf::from("/repo/skipped.yaml")),
            "unresolved",
        )],
    };
    UpdatePlan::from_report(report, Path::new("/repo"))
}
