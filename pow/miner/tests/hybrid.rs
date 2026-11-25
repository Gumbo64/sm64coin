//! Hybrid mining: a segment played by hand from a node (scripted inputs here) joins the search's tree as a
//! manual node, and replaying the path from the root through the verifier's own mining game (same block
//! perturbation) reaches exactly its state; so a run that uses the node is a valid proof.

use obrrt_core::{GamePad, read_state};
use obrrt_objectives::Registry;
use obrrt_search::{Config, Engine};
use obrrt_search::engine::ManualInsert;
use sm64coin_miner::hook::{hooked, hooked_save, mining_save};
use sm64pow::{Seed, SM64MiningGame, new_mining_game, params};
use std::sync::Arc;

/// Scripted "human" inputs: some A/B presses and stick movements, the same for a given `salt`.
fn script(salt: u32, n: usize) -> Vec<GamePad> {
    (0..n as u32)
        .map(|i| {
            let t = i.wrapping_mul(2654435761).wrapping_add(salt.wrapping_mul(40503));
            let button = if (i / 7 + salt) % 5 == 0 { 0x8000 } else if (i / 11) % 4 == 0 { 0x1000 } else { 0 };
            GamePad { button, stick_x: ((t >> 8) as i8 as i32 / 2) as i8, stick_y: (((t >> 16) as i8) as i32 / 2) as i8 }
        })
        .collect()
}

fn replay(mut g: SM64MiningGame, pads: &[GamePad]) -> SM64MiningGame {
    for p in pads {
        g.step_game(*p).unwrap();
    }
    g
}

#[test]
fn manual_nodes_replay_from_the_root() {
    if sm64pow::init_default().is_err() {
        eprintln!("no ROM; skipped");
        return;
    }
    let seed = Seed([9u8; 32]);
    // Play a few hundred frames of the intro so the root is a mid-game state with the perturbation running.
    let mut mine = new_mining_game(&seed, &params(1)).unwrap();
    for p in script(1, 400) {
        mine.step_game(p).unwrap();
    }
    let root_game = mine.fork().unwrap();
    let root = read_state(&mine, 400, true);
    let out = std::env::temp_dir().join(format!("hybrid-test-{}", std::process::id()));
    std::fs::create_dir_all(&out).unwrap();
    let mut cfg = Config::default();
    cfg.out_dir = out.to_string_lossy().into_owned();
    cfg.seed = 1;
    let save = mine.save_state();
    let mut engine = Engine::new_hooked(cfg, &hooked(&mine).unwrap(), 1, Vec::new(), hooked_save(&save), root, Arc::new(Registry)).unwrap();

    // 1. A human segment of 240 frames from the root node.
    let seg1 = script(2, 240);
    let a = match engine.insert_manual_at(0, &seg1, 8.0).unwrap() {
        ManualInsert::Added(n) => n,
        other => panic!("not added: {other:?}"),
    };
    assert_eq!(engine.manual_boosts(), vec![(a, 8.0)]);
    assert_eq!(engine.tree.nodes[a as usize].via, "manual");
    assert_eq!(engine.tree.nodes[a as usize].inputs.len(), seg1.len(), "the edge is the played inputs");
    // Inserting the same inputs again changes nothing.
    assert_eq!(engine.insert_manual_at(0, &seg1, 8.0).unwrap(), ManualInsert::Known);

    // 2. A second segment played from the first manual node's state (its state exported as the page gets it).
    let seg2 = script(3, 180);
    let b = match engine.insert_manual_at(a, &seg2, 8.0).unwrap() {
        ManualInsert::Added(n) => n,
        other => panic!("not added: {other:?}"),
    };
    assert_eq!(engine.tree.nodes[b as usize].parent, Some(a));

    // The state the page would load for node `a` is the state the verifier's game reaches there.
    let sa = mining_save(&engine.node_state(a).unwrap()).unwrap();
    let ga = replay(root_game.fork().unwrap(), &seg1);
    assert!(sa.game == ga.game.save_state(), "node a's exported memory equals the replay");
    assert_eq!(serde_json::to_string(&sa.pert).unwrap(), serde_json::to_string(&ga.pert).unwrap(), "and its perturbation");

    // The miner later replays the tree path root -> b through its mining game: the identical state, frame
    // and position as the node recorded.
    let path = engine.node_path(b);
    assert_eq!(path.len(), seg1.len() + seg2.len());
    let gb = replay(root_game.fork().unwrap(), &path);
    let sb = mining_save(&engine.node_state(b).unwrap()).unwrap();
    assert!(sb.game == gb.game.save_state(), "node b's memory equals the verifier's replay of the path");
    assert_eq!(serde_json::to_string(&sb.pert).unwrap(), serde_json::to_string(&gb.pert).unwrap());
    let st = read_state(&gb, 400 + path.len() as u32, true);
    let nb = &engine.tree.nodes[b as usize].st;
    assert_eq!((st.frame, st.level, st.area, st.pos, st.progress()), (nb.frame, nb.level, nb.area, nb.pos, nb.progress()));

    // A path given from the root (as the other workers get it: the parent's path then the segment) lands on the same node.
    let mut full = engine.node_path(a);
    full.extend_from_slice(&seg2);
    assert_eq!(engine.insert_manual(&full, 8.0).unwrap(), ManualInsert::Known);
    let _ = std::fs::remove_dir_all(&out);
}
