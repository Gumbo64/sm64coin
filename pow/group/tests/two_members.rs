use sm64coin_group::{blocking, Opts};
use std::sync::Arc;
use std::time::Duration;

const T: Duration = Duration::from_secs(30);

fn tpl(blob: &str) -> String {
    format!(r#"{{"blocktemplate_blob":"{blob}","height":5}}"#)
}

fn wait<F: Fn() -> bool>(what: &str, f: F) {
    let t0 = std::time::Instant::now();
    while !f() {
        assert!(t0.elapsed() < T, "timeout waiting for {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn leader_and_two_members_share_template_step_and_run() {
    let o = |n: &str| Opts { name: n.into(), relay: Some("none".into()), unverified_ok: false, platform: String::new() };
    let leader = blocking::create(o("leader")).unwrap();
    assert!(leader.code().starts_with("sm64team1-"));
    leader.publish_template(&tpl("aa")).unwrap();
    let a = blocking::join(leader.code(), o("alice")).unwrap();
    // A late joiner gets the current template.
    let (tid, json, g) = a.wait_template(0, T).expect("template reaches the member");
    assert!(json.contains("\"aa\""));
    assert_eq!(tid, sm64coin_group::template_id(&tpl("aa")));
    // Verifier: accept only runs whose first pad byte is 1.
    let ok = Arc::new(|_t: &str, r: &sm64coin_group::RunMsg| r.proof[0] == 1);
    leader.set_verifier(ok.clone());
    a.set_verifier(ok.clone());
    // A member puts a step: the leader (and later members) get it.
    a.put_step("k1", r#"{"key":"k1","out":{}}"#);
    wait("step at the leader", || leader.get_step("k1").is_some());
    let b = blocking::join(leader.code(), o("bob")).unwrap();
    b.set_verifier(ok.clone());
    b.wait_template(0, T).expect("late joiner template");
    wait("step at the late joiner", || b.get_step("k1").is_some());
    // A run: bad one dropped, good one held by everyone, shorter replaces.
    a.put_run("power-on", 4, &[0; 16], &["x".into()], &[0]);
    // put_run holds our own run locally; the others verify (pads[0]==0 fails).
    std::thread::sleep(Duration::from_secs(2));
    assert!(leader.best_run("power-on").is_none(), "unverified run must not be held");
    a.put_run("power-on", 3, &[1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], &["x".into()], &[0]);
    wait("good run at the leader", || leader.best_run("power-on").is_some_and(|r| r.frames == 3));
    wait("good run at bob", || b.best_run("power-on").is_some_and(|r| r.frames == 3));
    // Members list shows everyone.
    wait("members", || leader.members().len() == 3);
    // A member's own template is ignored (only the leader's are valid): a member cannot publish.
    assert!(a.publish_template(&tpl("bb")).is_err());
    // The leader moves on: a new template resets steps and runs for everyone.
    leader.publish_template(&tpl("cc")).unwrap();
    a.wait_template(g, T).expect("new template");
    assert!(a.get_step("k1").is_none());
    assert!(a.best_run("power-on").is_none());
    b.leave();
    wait("bob gone", || leader.members().len() == 2);
}
