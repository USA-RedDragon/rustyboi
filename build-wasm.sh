#!/bin/bash

set -euo pipefail

cd rustyboi-platform
wasm-pack build --target web --out-dir ../web/web-pack --no-opt
