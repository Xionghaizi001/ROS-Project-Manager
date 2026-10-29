use rospm_core::{
    confirmation,
    execution::{Confirmation, Event, Interaction, Operation},
    manifest::Manifest,
    mirror,
    plan::{Command, Plan, Step},
    platform::Platform,
    probe::Host,
    provider::Context,
    Error, Result,
};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

struct Handler<F>(F);
impl<F: Fn(&Confirmation) -> Result<bool> + Sync> Interaction for Handler<F> {
    fn event(&self, _: Event<'_>) {}
    fn confirm(&self, request: &Confirmation) -> Result<bool> {
        (self.0)(request)
    }
}

struct Fixture {
    root: PathBuf,
    manifest: Manifest,
    host: Host,
    plan: Plan,
}
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "rospm-confirm-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        for dir in ["ws/build", "ws/install", "ws/src", "deps_ws/build"] {
            fs::create_dir_all(root.join(dir)).unwrap();
        }
        fs::write(root.join("ws/build/cache"), "cache").unwrap();
        fs::write(root.join("ws/src/source"), "source").unwrap();
        let manifest =
            Manifest::parse(include_str!("fixtures/full.toml"), &root.join("rospm.toml")).unwrap();
        let mut plan = Plan::default();
        plan.note(rospm_core::Message::text("note"));
        plan.run(Command::new("colcon").arg("build").cwd(root.join("ws")));
        Self {
            root,
            manifest,
            host: Host::empty(),
            plan,
        }
    }
    fn ctx(&self) -> Context<'_> {
        Context {
            manifest: &self.manifest,
            root: &self.root,
            host: &self.host,
            platform: Platform::LinuxX86_64,
            locked: false,
            lock: None,
            offline: false,
            mirror: mirror::OFFICIAL,
        }
    }
    fn error(&self) -> Error {
        let Step::Run(c) = &self.plan.steps[1] else {
            unreachable!()
        };
        Error::CommandFailed {
            command: c.render(),
            status: "exit status: 2".into(),
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn mirror_scope_defaults_and_disconnect_are_explicit() {
    for snapshot in [false, true] {
        let ui = Handler(|r: &Confirmation| {
            assert_eq!(r.task, "apply-1");
            assert_eq!(r.step, 0);
            let Operation::SwitchMirror {
                name,
                ros_apt,
                rosdistro,
            } = &r.operation
            else {
                panic!()
            };
            assert_eq!(name, "aliyun");
            assert_eq!(ros_apt.is_none(), snapshot);
            assert_eq!(rosdistro, mirror::OFFICIAL_ROSDISTRO);
            assert!(r.accepts_answer(Some("\n")));
            assert!(!r.accepts_answer(None));
            assert!(!r.accepts_answer(Some("no")));
            Ok(true)
        });
        assert_eq!(
            confirmation::switch_mirror(
                "apply-1",
                mirror::ALIYUN,
                &[mirror::ALIYUN],
                snapshot,
                &ui
            )
            .unwrap(),
            mirror::ALIYUN
        );
    }
    assert!(confirmation::switch_mirror(
        "apply-2",
        mirror::TUNA,
        &[mirror::TUNA],
        false,
        &Handler(|_: &Confirmation| Ok(false))
    )
    .is_err());
    assert!(confirmation::switch_mirror(
        "apply-2",
        mirror::TUNA,
        &[mirror::TUNA],
        false,
        &Handler(|_: &Confirmation| Err(Error::io(
            "socket",
            std::io::ErrorKind::BrokenPipe.into()
        )))
    )
    .is_err());
}

#[test]
fn uninstall_executes_only_the_reviewed_plan_and_explicit_yes_bypasses_prompt() {
    let mut plan = Plan::default();
    plan.note(rospm_core::Message::text("scope"));
    plan.run(
        Command::new("apt-get")
            .args(["remove", "-y", "ros-jazzy-demo"])
            .sudo(),
    );
    let ui = Handler(|r: &Confirmation| {
        assert_eq!(r.task, "uninstall-1");
        assert_eq!(r.step, 2);
        let Operation::Uninstall { commands } = &r.operation else {
            panic!()
        };
        assert_eq!(commands[0].args, ["remove", "-y", "ros-jazzy-demo"]);
        assert!(!r.accepts_answer(None));
        assert!(!r.accepts_answer(Some("\n")));
        assert!(r.accepts_answer(Some("yes\n")));
        Ok(true)
    });
    let mut calls = 0;
    confirmation::uninstall("uninstall-1", &plan, false, &ui, |reviewed| {
        assert!(std::ptr::eq(reviewed, &plan));
        calls += 1;
        Ok(())
    })
    .unwrap();
    assert_eq!(calls, 1);
    assert!(confirmation::uninstall(
        "no",
        &plan,
        false,
        &Handler(|_: &Confirmation| Ok(false)),
        |_| panic!("declined")
    )
    .is_err());
    assert!(confirmation::uninstall(
        "gone",
        &plan,
        false,
        &Handler(|_: &Confirmation| Err(Error::io(
            "socket",
            std::io::ErrorKind::BrokenPipe.into()
        ))),
        |_| panic!("disconnected")
    )
    .is_err());
    confirmation::uninstall(
        "yes",
        &plan,
        true,
        &Handler(|_: &Confirmation| panic!("explicit yes")),
        |_| Ok(()),
    )
    .unwrap();
    confirmation::uninstall(
        "empty",
        &Plan::default(),
        false,
        &Handler(|_: &Confirmation| panic!("empty")),
        |_| panic!("empty"),
    )
    .unwrap();
}

#[test]
fn clean_retry_shows_full_selected_scope_and_runs_once_after_deletion() {
    let f = Fixture::new();
    // Internal symlinks must not cause recursive deletion outside the scope.
    std::os::unix::fs::symlink(f.root.join("ws/src"), f.root.join("ws/build/link")).unwrap();
    let ui = Handler(|r: &Confirmation| {
        assert_eq!(r.task, "build-1");
        assert_eq!(r.step, 2);
        let Operation::CleanBuild { paths, retry } = &r.operation else {
            panic!()
        };
        assert_eq!(paths, &[f.root.join("ws/build"), f.root.join("ws/install")]);
        assert!(paths.iter().all(|p| p.is_absolute()));
        assert_eq!(
            retry[0].render(),
            match &f.plan.steps[1] {
                Step::Run(c) => c.render(),
                _ => unreachable!(),
            }
        );
        assert_eq!(
            r.explanation.as_ref().unwrap().key_name(),
            Some("cli.build.clean_offer")
        );
        assert!(!r.accepts_answer(None));
        assert!(!r.accepts_answer(Some("")));
        Ok(true)
    });
    let mut calls = 0;
    assert!(confirmation::retry_build(
        "build-1",
        &f.plan,
        &f.ctx(),
        Some("main"),
        &f.error(),
        &ui,
        |plan| {
            calls += 1;
            assert!(std::ptr::eq(plan, &f.plan));
            assert!(!f.root.join("ws/build").exists());
            assert!(!f.root.join("ws/install").exists());
            Ok(())
        }
    )
    .unwrap());
    assert_eq!(calls, 1);
    assert!(f.root.join("deps_ws/build").is_dir());
    assert_eq!(
        fs::read_to_string(f.root.join("ws/src/source")).unwrap(),
        "source"
    );
}

#[test]
fn declined_disconnected_and_unrelated_failures_never_clean_or_retry() {
    let f = Fixture::new();
    assert!(!confirmation::retry_build(
        "deny",
        &f.plan,
        &f.ctx(),
        None,
        &f.error(),
        &Handler(|_: &Confirmation| Ok(false)),
        |_| panic!("declined")
    )
    .unwrap());
    assert!(confirmation::retry_build(
        "gone",
        &f.plan,
        &f.ctx(),
        None,
        &f.error(),
        &Handler(|_: &Confirmation| Err(Error::io(
            "socket",
            std::io::ErrorKind::UnexpectedEof.into()
        ))),
        |_| panic!("disconnected")
    )
    .is_err());
    assert!(!confirmation::retry_build(
        "other",
        &f.plan,
        &f.ctx(),
        None,
        &Error::Host(rospm_core::Message::text("other")),
        &Handler(|_: &Confirmation| panic!("unrelated")),
        |_| panic!("unrelated")
    )
    .unwrap());
    assert!(f.root.join("ws/build/cache").exists());
}

#[test]
fn changed_directory_rejects_entire_cleanup_before_any_deletion() {
    let f = Fixture::new();
    let ui = Handler(|_: &Confirmation| {
        fs::rename(f.root.join("ws/install"), f.root.join("original-install")).unwrap();
        fs::create_dir(f.root.join("ws/install")).unwrap();
        Ok(true)
    });
    let error = confirmation::retry_build(
        "changed",
        &f.plan,
        &f.ctx(),
        Some("main"),
        &f.error(),
        &ui,
        |_| panic!("changed"),
    )
    .unwrap_err();
    assert_eq!(
        error.message().key_name(),
        Some("confirmation.directory_changed")
    );
    assert!(f.root.join("ws/build/cache").exists());
    assert!(f.root.join("ws/install").exists());
}

#[test]
fn project_source_and_symlink_roots_are_rejected() {
    for base in [".", "..", "src", "src/sub", "link", "link/"] {
        let mut f = Fixture::new();
        fs::create_dir(f.root.join("ws/src/sub")).unwrap();
        std::os::unix::fs::symlink(f.root.join("ws/build"), f.root.join("ws/link")).unwrap();
        f.manifest.workspaces["main"].colcon.build_args = vec!["--build-base".into(), base.into()];
        assert!(
            confirmation::retry_build(
                "unsafe",
                &f.plan,
                &f.ctx(),
                Some("main"),
                &f.error(),
                &Handler(|_: &Confirmation| panic!("unsafe scope")),
                |_| panic!("unsafe scope")
            )
            .is_err(),
            "{base}"
        );
        assert!(f.root.join("ws/src/source").exists());
    }
}

#[test]
fn nested_bases_are_removed_once_and_retry_failure_is_not_retried_again() {
    let mut f = Fixture::new();
    fs::create_dir(f.root.join("ws/build/install")).unwrap();
    f.manifest.workspaces["main"].colcon.build_args = vec!["--install-base=build/install".into()];
    let ui = Handler(|r: &Confirmation| {
        let Operation::CleanBuild { paths, .. } = &r.operation else {
            panic!()
        };
        assert_eq!(paths, &[f.root.join("ws/build")]);
        Ok(true)
    });
    let mut calls = 0;
    assert!(confirmation::retry_build(
        "nested",
        &f.plan,
        &f.ctx(),
        Some("main"),
        &f.error(),
        &ui,
        |_| {
            calls += 1;
            Err(f.error())
        }
    )
    .is_err());
    assert_eq!(calls, 1);
    assert!(!f.root.join("ws/build").exists());
    assert!(f.root.join("ws/install").exists());
}

#[test]
fn no_outputs_or_unreviewable_uninstall_never_execute() {
    let f = Fixture::new();
    fs::remove_dir_all(f.root.join("ws/build")).unwrap();
    fs::remove_dir_all(f.root.join("ws/install")).unwrap();
    assert!(!confirmation::retry_build(
        "no-outputs",
        &f.plan,
        &f.ctx(),
        Some("main"),
        &f.error(),
        &Handler(|_: &Confirmation| panic!("no outputs")),
        |_| panic!("no outputs")
    )
    .unwrap());
    let plan = Plan {
        steps: vec![Step::WriteFile {
            path: f.root.join("unexpected"),
            contents: "x".into(),
            sudo: false,
        }],
        ..Plan::default()
    };
    assert!(confirmation::uninstall(
        "unsupported",
        &plan,
        true,
        &Handler(|_: &Confirmation| panic!("unsupported")),
        |_| panic!("unsupported")
    )
    .is_err());
}
