#!/bin/bash
export PATH="$HOME/.cargo/bin:$PATH" CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=4 CARGO_TARGET_DIR=/home/mike/repos/roost-v3-worker/target-track
cd /home/mike/repos/roost-v3-worker
exec flock /home/mike/repos/roost-v3-worker/target-track/.roost-build.lock /home/mike/repos/roost-build-slot cargo "$@"
