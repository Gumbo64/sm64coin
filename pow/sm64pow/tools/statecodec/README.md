# State codec tooling

- `cargo run --release --example statedump -- OUT.bin m64 FILE...` replays power-on movies and dumps per-frame (features, pad) rows; `... run RUN.json...` does the same for proof runs.
- `dumpio.py` reads those dumps (numpy).
- `cargo run --release --example statecodec_eval -- DUMP.bin WINDOW [sim [PRIOR.bin,... [limit n0 limit2]]]` bits/frame of raw/rle/xz/planar+delta-xz/statectx1 on windows, optionally with simulated pokes and an in-memory trained prior (experiment; the shipped codec 3 has no trained table).
- `cargo run --release --example statecodec_bench -- RUN.json...` proof bytes per codec and replay timing.
