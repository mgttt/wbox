#!/bin/sh
# guest 测试 CI 入口（契约：docs/testing.md §2 / .github/workflows/ci.yml guest-tests job）。
# 包装 tests/run-guest-tests.sh：透传全部参数，退出码原样转发（退出码即测试结果）。
exec "$(dirname "$0")/run-guest-tests.sh" "$@"
