#!/bin/sh
# guest 测试 CI 入口（契约：docs/testing.md §2 / .github/workflows/ci.yml guest-tests job）。
# 包装 tests/run-guest-tests.sh：透传全部参数，退出码原样转发（退出码即测试结果）。
# 显式用 bash 调用，不依赖可执行位：仓库里这些脚本曾是 100644，
# Linux 上 `bash tests/run.sh` 会以 "Permission denied" 失败（Windows/msys2
# 模拟可执行位故不显形）。可执行位已同时补上，这里再加一层保险；
# 且 run-guest-tests.sh 用了 bash 数组，必须 bash 而非 sh。
exec bash "$(dirname "$0")/run-guest-tests.sh" "$@"
