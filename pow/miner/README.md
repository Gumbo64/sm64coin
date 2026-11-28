# sm64coin-miner

The SM64Coin auto-miner: plays Super Mario 64 with a tree search (the RRT campaign over
[sm64-AI-binds](https://github.com/Gumbo64/sm64-AI-binds)'s `obrrt-*` crates, pinned by git rev in Cargo.toml) to the
next progress point (star, key, cap switch) of a block's start state, seals the run as a proof (`sm64pow`) and submits
it. Validity never depends on the search: the miner's shadow replay (`rrt/mod.rs`) plays the block for real through the
perturbed game and `sm64pow::seal_proof_at` seals exactly those pads, so any search that finds a route mines.

## Binaries

| binary | what |
| --- | --- |
| `automine` | the miner CLI (desktop sidecar), a thin command line over `campaign::Campaign` (the one mining loop): template -> search -> seal -> submit. The search steps run on threads of the process; `--process` runs each in an `rrtrun` process instead (crash and memory isolation). `--viz` adds the tree feed on stdout/stdin. |
| `rrtrun` | one search step as its own process (`rrtrun <job.json> <out.json>`; what `automine --process` spawns). `rrtrun --inproc ...` runs the same job through the in-process runner (the gate compares the two byte for byte). |
| `rrtbench`, `rhbench` | benchmarks (`rhbench`, and the SDK's `make_fixture`, use the rolling-horizon GA of `rh.rs`, which also owns the boot/settle macros) |

## Step runners (`rrt/child.rs`)

A step's settings are one typed `obrrt_core::config::SearchConfig` built by `child::step_config(job)`: the job's
`OBRRT_*` list (the campaign preset: `rrt::env`, now only the switches that differ from the search's defaults) through the
`obrrt-env` adapter, plus the step's own fields (skip list). Nothing reads the process environment, so:

* `child::run_step(&job)` - as the process's only step (`rrtrun`, a fresh browser instance): settings installed
  process-wide, global rayon pool.
* `child::run_step_inproc(&job, &StepIo)` - on the calling thread, inside a process that runs other things: the config is
  scoped on the step's threads, the engine gets its own `Config::runtime` and the step its own rayon pool of `job.threads`.
  `StepIo` carries the viz/tree-sync exchange and a stop flag (the engine's next round ends the search).

`rrt::RrtOptions::in_process` switches `rrt::mine` from the first to the second. It needs `sm64pow::init(rom_bytes)` done by
the caller (no ROM file is read per step).

## Library API for apps: `sm64coin_miner::campaign`

No child process, no filesystem outside `data_dir`, ROM taken as bytes. One campaign per process.

```rust
use sm64coin_miner::campaign::{Campaign, CampaignConfig, Event};

let rom: Vec<u8> = /* the app's stored ROM */;
let mut cfg = CampaignConfig::new("http://127.0.0.1:16481", "SM64...", rom, data_dir);
cfg.threads = 4;          // search threads (rayon pool per step)
cfg.viz = true;           // Event::Viz frames (the tree feed)
let c = Campaign::start(cfg)?;          // returns at once; the loop runs on its own thread
let rx = c.subscribe();                 // std mpsc Receiver<Event>; one per call
c.mailbox(r#"{"viz_on":true}"#);        // the page's switch / bubble / node requests, as on the desktop's stdin
let s = c.stats();                      // {running, frames, fps (since the last stats() call), blocks_sent, blocks_rejected, height, phase}
c.stop();  c.join();                    // stop ends the running search at its next round
```

| item | |
| --- | --- |
| `CampaignConfig::new(daemon_url, address, rom_bytes, data_dir)` | the rest has defaults: `threads 4`, `viz false`, `reset "when-stuck"`, `anytime true`, `normalise Exact`, `submit_lead 5`, `hub_frames`/`course_frames` as automine, `payout_view_secret None`, `max_blocks 0`, `in_process true` (false: an `rrtrun` process per step, needs `rom_path`), and the rest of automine's flags (`scratch`, `signed_file`, `reset_after`, `stamp_ahead`, `save_block`, `save_dir`, ...) |
| `Campaign::start / stop / join / subscribe / state / stats / mailbox` | as above; dropping a `Campaign` stops and joins it |
| `Event::Log(String)` | the miner's log lines (the search engine's own console output is not captured) |
| `Event::State(State{phase, height, detail})` | `Phase`: Starting, WaitingForDaemon, Preparing, Searching, Improving, Sealing, WaitingValid, Idle, Stopped |
| `Event::Viz{kind, key, payload}` | tree-feed frames, kinds `viz`, `state`, `edges`, `pert`, `path`, `hybrid_out` (payloads as in rrt/vizio.rs: JSON, or base64 for `state`/`edges`) |
| `Event::Block(BlockEvent)` | a submitted block: height, frames, accepted, held_secs, message |
| `Event::Stopped(Option<String>)` | the loop ended (None: stop()/max_blocks; Some: the error) |

Files written, all under `data_dir`: `signed.json` (one signed run per seed, kept across restarts: never two different runs
for a seed), `rrt-scratch/` (campaign memory, roadmap, priors, step files; steps delete their own). The campaign is the
loop `automine` runs (template, parent end state or power-on by the reset policy, anytime search for a shorter run
while the run waits for the time limit, normalise, sign once, submit); `examples/campaign.rs` is a runnable driver.

Android: `cargo build --release --target aarch64-linux-android -p sm64coin-miner --lib --examples` builds and links (NDK 27, the
clang of `aarch64-linux-android24` as `CC_aarch64_linux_android`, `AR_aarch64_linux_android` and the cargo linker; `ring`, from
`ureq`'s rustls, compiles C). Not run on a device: whether the wasmer JIT in sm64-binds runs under Android's policies is untested.

## Hard-star policy (`scripts/automine-fast.sh`)

The campaign does not need to commit to a hard star. The wrapper runs the tuned policy that mined the first blocks on the
live v0.5 chain when a 150 M-frame commitment to BoB found nothing: two 20 M-frame course budgets, two checkpoints, a 45 s
wall-clock cap per tree, and no cap-switch courses. A course that gives no progress is skipped and the next one is tried;
the campaign's `campaign.json` memory keeps the skip list across blocks. `--no-caps` avoids a cap switch: the switch is a
valid block but leaves the next block inside the secret course, where the pause-menu exit can fail (the miner now waits
for the switch cutscene to finish and exits by the pause menu; `rrt::pause_exit`). `--rrt-target <level>` forces one course
(a portfolio of miners can search different courses in parallel); `--rrt-course-frames`, `--rrt-checkpoints`,
`--rrt-step-seconds` override the preset.

```sh
scripts/automine-fast.sh --address SM64... --daemon http://127.0.0.1:16481 --rom baserom.us.z64 --rh-threads 15 --blocks 1
```
