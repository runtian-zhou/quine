#!/usr/bin/env bash
# QA test runner for the quine CLI.
#
# Spawns a daemon, runs test cases from test_cases.json, and collects
# reports into qa/reports/.
#
# Usage: ./qa/run_qa.sh [--socket /path/to/socket]
#
# Requires: jq, cargo (quine must be built)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
REPORT_DIR="${SCRIPT_DIR}/reports"
TEST_CASES="${SCRIPT_DIR}/test_cases.json"
SOCKET="${1:-/tmp/quine-qa-test.sock}"
QUINE_BIN="${PROJECT_ROOT}/target/debug/quine"
TIMESTAMP="$(date -u +%Y%m%dT%H%M%SZ)"
REPORT_FILE="${REPORT_DIR}/qa-${TIMESTAMP}.json"

# Ensure binary is built.
if [ ! -f "${QUINE_BIN}" ]; then
    echo "Building quine..."
    cargo build --manifest-path "${PROJECT_ROOT}/Cargo.toml"
fi

# Clean up on exit.
cleanup() {
    if [ -n "${DAEMON_PID:-}" ]; then
        echo "Stopping daemon (PID ${DAEMON_PID})..."
        "${QUINE_BIN}" daemon stop --socket "${SOCKET}" 2>/dev/null || true
        kill "${DAEMON_PID}" 2>/dev/null || true
        wait "${DAEMON_PID}" 2>/dev/null || true
    fi
    rm -f "${SOCKET}"
}
trap cleanup EXIT

# Start daemon.
echo "Starting daemon on ${SOCKET}..."
"${QUINE_BIN}" daemon start --socket "${SOCKET}" &
DAEMON_PID=$!
sleep 2

# Verify daemon is running.
if ! kill -0 "${DAEMON_PID}" 2>/dev/null; then
    echo "ERROR: Daemon failed to start"
    exit 1
fi

mkdir -p "${REPORT_DIR}"

# Run test cases.
TOTAL=0
PASSED=0
FAILED=0
RESULTS="[]"

NUM_CASES=$(jq 'length' "${TEST_CASES}")

for i in $(seq 0 $((NUM_CASES - 1))); do
    CASE_NAME=$(jq -r ".[$i].name" "${TEST_CASES}")
    CASE_DESC=$(jq -r ".[$i].description" "${TEST_CASES}")
    JSON_MODE=$(jq -r ".[$i].json_mode // false" "${TEST_CASES}")
    NUM_TURNS=$(jq ".[$i].turns | length" "${TEST_CASES}")

    echo "--- Test: ${CASE_NAME} ---"
    echo "  ${CASE_DESC}"

    SESSION_ID=""
    CASE_PASSED=true
    TURN_RESULTS="[]"

    for t in $(seq 0 $((NUM_TURNS - 1))); do
        MSG=$(jq -r ".[$i].turns[$t].message" "${TEST_CASES}")
        EXPECT=$(jq -r ".[$i].turns[$t].expect_contains // empty" "${TEST_CASES}")

        TOTAL=$((TOTAL + 1))

        # Build command.
        CMD_ARGS=("run")
        if [ "${JSON_MODE}" = "true" ]; then
            CMD_ARGS+=("--json")
        fi
        CMD_ARGS+=("--socket" "${SOCKET}")
        if [ -n "${SESSION_ID}" ]; then
            CMD_ARGS+=("--session" "${SESSION_ID}")
        fi
        CMD_ARGS+=("${MSG}")

        # Run the command, capture stdout and stderr.
        STDOUT_FILE=$(mktemp)
        STDERR_FILE=$(mktemp)

        if "${QUINE_BIN}" "${CMD_ARGS[@]}" >"${STDOUT_FILE}" 2>"${STDERR_FILE}"; then
            OUTPUT=$(cat "${STDOUT_FILE}")
            STDERR_OUTPUT=$(cat "${STDERR_FILE}")

            # Extract session_id from stderr if this is the first turn.
            if [ -z "${SESSION_ID}" ] && [ -n "${STDERR_OUTPUT}" ]; then
                SESSION_ID=$(echo "${STDERR_OUTPUT}" | grep -o 'session: [^ ]*' | cut -d' ' -f2 || true)
            fi

            # Check expectations.
            TURN_PASS=true
            if [ -n "${EXPECT}" ]; then
                if ! echo "${OUTPUT}" | grep -q "${EXPECT}"; then
                    echo "  FAIL turn ${t}: expected '${EXPECT}' in output"
                    TURN_PASS=false
                fi
            fi

            # Check JSON fields if applicable.
            EXPECT_JSON=$(jq -r ".[$i].turns[$t].expect_json_fields // empty" "${TEST_CASES}")
            if [ -n "${EXPECT_JSON}" ] && [ "${EXPECT_JSON}" != "null" ]; then
                for field in $(echo "${EXPECT_JSON}" | jq -r '.[]'); do
                    if ! echo "${OUTPUT}" | jq -e ".${field}" >/dev/null 2>&1; then
                        echo "  FAIL turn ${t}: missing JSON field '${field}'"
                        TURN_PASS=false
                    fi
                done
            fi

            if [ "${TURN_PASS}" = "true" ]; then
                echo "  PASS turn ${t}"
                PASSED=$((PASSED + 1))
            else
                CASE_PASSED=false
                FAILED=$((FAILED + 1))
            fi

            TURN_RESULTS=$(echo "${TURN_RESULTS}" | jq \
                --arg msg "${MSG}" \
                --arg out "${OUTPUT}" \
                --argjson pass "${TURN_PASS}" \
                '. + [{"message": $msg, "output": $out, "passed": $pass}]')
        else
            echo "  FAIL turn ${t}: command failed"
            CASE_PASSED=false
            FAILED=$((FAILED + 1))
            TURN_RESULTS=$(echo "${TURN_RESULTS}" | jq \
                --arg msg "${MSG}" \
                '. + [{"message": $msg, "output": "", "passed": false, "error": "command failed"}]')
        fi

        rm -f "${STDOUT_FILE}" "${STDERR_FILE}"
    done

    RESULTS=$(echo "${RESULTS}" | jq \
        --arg name "${CASE_NAME}" \
        --arg desc "${CASE_DESC}" \
        --argjson passed "${CASE_PASSED}" \
        --argjson turns "${TURN_RESULTS}" \
        '. + [{"name": $name, "description": $desc, "passed": $passed, "turns": $turns}]')
done

# Write report.
REPORT=$(jq -n \
    --arg ts "${TIMESTAMP}" \
    --argjson total "${TOTAL}" \
    --argjson passed "${PASSED}" \
    --argjson failed "${FAILED}" \
    --argjson results "${RESULTS}" \
    '{"timestamp": $ts, "total": $total, "passed": $passed, "failed": $failed, "results": $results}')

echo "${REPORT}" | jq . > "${REPORT_FILE}"

echo ""
echo "=== QA Results ==="
echo "Total: ${TOTAL}, Passed: ${PASSED}, Failed: ${FAILED}"
echo "Report: ${REPORT_FILE}"

if [ "${FAILED}" -gt 0 ]; then
    exit 1
fi
