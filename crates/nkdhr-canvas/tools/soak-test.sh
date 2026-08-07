#!/usr/bin/env bash

set -euo pipefail

readonly SCRIPT_NAME="soak-test.sh"
readonly SCRIPT_PATH="$(realpath "${BASH_SOURCE[0]}")"
readonly TOOLS_DIR="$(dirname "${SCRIPT_PATH}")"
readonly REPO_ROOT="$(realpath "${TOOLS_DIR}/../../..")"
readonly STATE_ROOT="${NKDHR_SOAK_STATE_ROOT:-${XDG_STATE_HOME:-${HOME}/.local/state}/nkdhr/soak}"

umask 077

usage() {
    cat <<'EOF'
Usage: soak-test.sh run [--duration 8h] [--interval 30s] [--session ID]
                        [--] [canvas-command ...]
       soak-test.sh start --pid PID [--duration 8h] [--interval 30s]
                          [--session ID]
       soak-test.sh status [RUN_ID]
       soak-test.sh stop [RUN_ID]
       soak-test.sh report [RUN_ID]
       soak-test.sh self-test

Runs the detached COMP-8 stability collector. `run` starts the collector and
then execs target/release/nkdhr-canvas --tty by default, preserving the local
controlling TTY. `start` attaches to an existing compositor. Only time during
which the recorded logind session is active counts toward the duration.

Runtime data is stored below:
  $XDG_STATE_HOME/nkdhr/soak
or, when XDG_STATE_HOME is unset:
  ~/.local/state/nkdhr/soak

`stop` ends collection without terminating the monitored compositor.
EOF
}

die() {
    echo "${SCRIPT_NAME}: $*" >&2
    exit 1
}

now_iso() {
    date --iso-8601=seconds
}

atomic_write() {
    local path=$1
    local value=$2
    local temporary="${path}.tmp.$$"

    printf '%s\n' "${value}" >"${temporary}"
    mv -f -- "${temporary}" "${path}"
}

parse_duration() {
    local value=$1
    local amount
    local suffix

    if [[ ! ${value} =~ ^([1-9][0-9]*)([smh]?)$ ]]; then
        die "invalid duration '${value}'; use a positive value such as 30s, 10m, or 8h"
    fi
    amount=${BASH_REMATCH[1]}
    suffix=${BASH_REMATCH[2]}
    case ${suffix} in
        "" | s) printf '%s\n' "${amount}" ;;
        m) printf '%s\n' "$((amount * 60))" ;;
        h) printf '%s\n' "$((amount * 3600))" ;;
    esac
}

format_duration() {
    local seconds=$1
    printf '%02d:%02d:%02d' "$((seconds / 3600))" "$(((seconds % 3600) / 60))" "$((seconds % 60))"
}

process_start_time() {
    local pid=$1
    local stat_line
    local remainder
    local -a fields

    IFS= read -r stat_line <"/proc/${pid}/stat" || return 1
    remainder=${stat_line##*) }
    read -r -a fields <<<"${remainder}"
    [[ ${#fields[@]} -gt 19 ]] || return 1
    printf '%s\n' "${fields[19]}"
}

process_command() {
    local pid=$1
    tr '\0' ' ' <"/proc/${pid}/cmdline" 2>/dev/null | sed 's/[[:space:]]*$//'
}

detect_session() {
    local candidate
    local controlling_tty

    candidate=${XDG_SESSION_ID:-}
    if [[ -n ${candidate} ]] \
        && [[ $(loginctl show-session "${candidate}" -p TTY --value 2>/dev/null) == tty[0-9]* ]]; then
        printf '%s\n' "${candidate}"
        return
    fi

    controlling_tty=$(readlink "/proc/$$/fd/0" 2>/dev/null || true)
    controlling_tty=${controlling_tty##*/}
    if [[ ${controlling_tty} == tty[0-9]* ]]; then
        while read -r candidate; do
            [[ -n ${candidate} ]] || continue
            if [[ $(loginctl show-session "${candidate}" -p TTY --value 2>/dev/null) == "${controlling_tty}" ]]; then
                printf '%s\n' "${candidate}"
                return
            fi
        done < <(loginctl list-sessions --no-legend 2>/dev/null | awk -v uid="$(id -u)" '$2 == uid { print $1 }')
    fi

    candidate=$(loginctl show-seat seat0 -p ActiveSession --value 2>/dev/null || true)
    if [[ -n ${candidate} ]]; then
        printf '%s\n' "${candidate}"
        return
    fi
    return 1
}

session_is_active() {
    local session_id=$1
    [[ $(loginctl show-session "${session_id}" -p Active --value 2>/dev/null) == yes ]]
}

metadata_value() {
    local run_dir=$1
    local key=$2
    awk -F= -v key="${key}" '$1 == key { sub(/^[^=]*=/, ""); print; exit }' "${run_dir}/metadata.txt"
}

resolve_run_dir() {
    local run_id=${1:-}

    if [[ -z ${run_id} ]]; then
        [[ -f ${STATE_ROOT}/latest ]] || die "no soak run has been recorded"
        run_id=$(<"${STATE_ROOT}/latest")
    fi
    [[ ${run_id} =~ ^[A-Za-z0-9_.-]+$ ]] || die "invalid run id '${run_id}'"
    [[ -d ${STATE_ROOT}/${run_id} ]] || die "unknown run id '${run_id}'"
    printf '%s\n' "${STATE_ROOT}/${run_id}"
}

clear_active_run() {
    local run_id=$1
    if [[ -f ${STATE_ROOT}/active ]] && [[ $(<"${STATE_ROOT}/active") == "${run_id}" ]]; then
        rm -f -- "${STATE_ROOT}/active"
    fi
}

event_log() {
    local run_dir=$1
    local kind=$2
    shift 2
    printf '%s\t%s\t%s\n' "$(now_iso)" "${kind}" "$*" >>"${run_dir}/events.log"
}

output_signature() {
    local connector
    local enabled
    local signature=""
    local status

    for connector in /sys/class/drm/card*-*; do
        [[ -f ${connector}/status ]] || continue
        status=$(<"${connector}/status")
        enabled=unknown
        if [[ -f ${connector}/enabled ]]; then
            enabled=$(<"${connector}/enabled")
        fi
        signature+="$(basename "${connector}"):${status}:${enabled};"
    done
    printf '%s\n' "${signature:--}"
}

process_metrics() {
    local pid=$1
    local status_file="/proc/${pid}/status"
    local stat_line
    local remainder
    local -a fields
    local fd
    local fd_count=0
    local rss=-1
    local hwm=-1
    local threads=-1
    local cpu_ticks=-1

    if [[ -r ${status_file} ]]; then
        read -r rss hwm threads < <(
            awk '
                $1 == "VmRSS:" { rss = $2 }
                $1 == "VmHWM:" { hwm = $2 }
                $1 == "Threads:" { threads = $2 }
                END { print rss + 0, hwm + 0, threads + 0 }
            ' "${status_file}"
        )
    fi
    if IFS= read -r stat_line <"/proc/${pid}/stat" 2>/dev/null; then
        remainder=${stat_line##*) }
        read -r -a fields <<<"${remainder}"
        if [[ ${#fields[@]} -gt 12 ]]; then
            cpu_ticks=$((${fields[11]} + ${fields[12]}))
        fi
    fi
    for fd in "/proc/${pid}/fd"/*; do
        [[ -e ${fd} || -L ${fd} ]] || continue
        fd_count=$((fd_count + 1))
    done
    printf '%s %s %s %s %s\n' "${rss}" "${hwm}" "${threads}" "${fd_count}" "${cpu_ticks}"
}

drm_metrics() {
    local pid=$1
    local -a fdinfo_files=("/proc/${pid}/fdinfo"/*)

    if [[ ! -e ${fdinfo_files[0]} ]]; then
        printf '%s\n' '0 0 0 0 0 0 0 0'
        return
    fi

    awk '
        function reset_file() {
            client = pdev = ""
            render = copy = video = enhance = total = resident = 0
        }
        function flush_file( key) {
            if (client == "") return
            key = pdev ":" client
            seen[key] = 1
            if (render > max_render[key]) max_render[key] = render
            if (copy > max_copy[key]) max_copy[key] = copy
            if (video > max_video[key]) max_video[key] = video
            if (enhance > max_enhance[key]) max_enhance[key] = enhance
            if (total > max_total[key]) max_total[key] = total
            if (resident > max_resident[key]) max_resident[key] = resident
        }
        FNR == 1 { flush_file(); reset_file() }
        $1 == "drm-client-id:" { client = $2 }
        $1 == "drm-pdev:" { pdev = $2 }
        $1 == "drm-engine-render:" { render = $2 }
        $1 == "drm-engine-copy:" { copy = $2 }
        $1 == "drm-engine-video:" { video = $2 }
        $1 == "drm-engine-video-enhance:" { enhance = $2 }
        $1 ~ /^drm-total-/ { total += $2 }
        $1 ~ /^drm-resident-/ { resident += $2 }
        END {
            flush_file()
            for (key in seen) {
                clients++
                sum_render += max_render[key]
                sum_copy += max_copy[key]
                sum_video += max_video[key]
                sum_enhance += max_enhance[key]
                sum_total += max_total[key]
                sum_resident += max_resident[key]
            }
            printf "%d %.0f %.0f %.0f %.0f %.0f %.0f %d\n", clients,
                sum_render, sum_copy, sum_video, sum_enhance,
                sum_total, sum_resident, (clients > 0)
        }
    ' "${fdinfo_files[@]}" 2>/dev/null || printf '%s\n' '0 0 0 0 0 0 0 0'
}

sample_process() {
    local run_dir=$1
    local pid=$2
    local expected_start=$3
    local active_seconds=$4
    local wall_seconds=$5
    local session_active=$6
    local alive=no
    local identity=no
    local rss=-1 hwm=-1 threads=-1 fd_count=-1 cpu_ticks=-1
    local drm_clients=0 drm_render=0 drm_copy=0 drm_video=0 drm_enhance=0
    local drm_total=0 drm_resident=0 drm_available=0
    local outputs

    if [[ -d /proc/${pid} ]]; then
        alive=yes
        if [[ $(process_start_time "${pid}" 2>/dev/null || true) == "${expected_start}" ]]; then
            identity=yes
            read -r rss hwm threads fd_count cpu_ticks < <(process_metrics "${pid}")
            read -r drm_clients drm_render drm_copy drm_video drm_enhance drm_total drm_resident drm_available < <(drm_metrics "${pid}")
        fi
    fi
    outputs=$(output_signature)
    printf '%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s\n' \
        "$(now_iso)" "${wall_seconds}" "${active_seconds}" "${session_active}" \
        "${alive}" "${identity}" "${rss}" "${hwm}" "${threads}" "${fd_count}" \
        "${cpu_ticks}" "${drm_clients}" "${drm_render}" "${drm_copy}" \
        "${drm_video}" "${drm_enhance}" "${drm_total}" "${drm_resident}" \
        "${drm_available}" "${outputs}" >>"${run_dir}/samples.csv"
}

capture_kernel_drm_log() {
    local run_dir=$1
    local start_epoch=$2
    local target="${run_dir}/kernel-drm.log"

    if ! journalctl -k --since "@${start_epoch}" --no-pager -o short-iso 2>/dev/null \
        | grep -Eai '((drm|i915).*(error|fail|hang|reset|timeout))|(GPU HANG)' \
        >"${target}"; then
        : >"${target}"
    fi
}

write_report() {
    local run_dir=$1
    local state
    local duration
    local active_seconds
    local start_epoch
    local stats
    local samples live_samples first_rss last_rss peak_hwm first_fd last_fd peak_fd
    local first_cpu last_cpu first_render last_render idle_intervals
    local rss_growth fd_growth kernel_errors warnings=0
    local automatic_verdict
    local report="${run_dir}/report.md"

    state=$(<"${run_dir}/state")
    duration=$(metadata_value "${run_dir}" duration_seconds)
    start_epoch=$(metadata_value "${run_dir}" start_epoch)
    active_seconds=$(<"${run_dir}/active_seconds")
    capture_kernel_drm_log "${run_dir}" "${start_epoch}"

    stats=$(awk -F, '
        NR > 1 {
            samples++
            if ($6 == "yes" && $7 >= 0) {
                live++
                if (live == 1) {
                    first_rss = $7; first_fd = $10; first_cpu = $11; first_render = $13
                }
                last_rss = $7; last_fd = $10; last_cpu = $11; last_render = $13
                if ($8 > peak_hwm) peak_hwm = $8
                if ($10 > peak_fd) peak_fd = $10
                if ($4 == "yes" && have_previous_render && $13 == previous_render) idle++
                previous_render = $13
                have_previous_render = 1
            }
        }
        END {
            printf "%d %d %d %d %d %d %d %d %.0f %.0f %.0f %.0f %d\n", samples,
                live, first_rss, last_rss, peak_hwm, first_fd, last_fd, peak_fd,
                first_cpu, last_cpu, first_render, last_render, idle
        }
    ' "${run_dir}/samples.csv")
    read -r samples live_samples first_rss last_rss peak_hwm first_fd last_fd peak_fd \
        first_cpu last_cpu first_render last_render idle_intervals <<<"${stats}"
    rss_growth=$((last_rss - first_rss))
    fd_growth=$((last_fd - first_fd))
    kernel_errors=$(wc -l <"${run_dir}/kernel-drm.log")

    case ${state} in
        completed) automatic_verdict=PASS ;;
        failed) automatic_verdict=FAIL ;;
        stopped) automatic_verdict=STOPPED ;;
        *) automatic_verdict=IN_PROGRESS ;;
    esac

    if ((first_rss > 0 && rss_growth > 65536 && rss_growth * 4 > first_rss)); then
        warnings=$((warnings + 1))
    fi
    if ((fd_growth > 32)); then
        warnings=$((warnings + 1))
    fi
    if ((kernel_errors > 0)); then
        warnings=$((warnings + 1))
    fi
    if ((samples > 1 && idle_intervals == 0)); then
        warnings=$((warnings + 1))
    fi
    if [[ ${automatic_verdict} == PASS && ${warnings} -gt 0 ]]; then
        automatic_verdict=PASS_WITH_WARNINGS
    fi

    {
        echo "# nkdhr COMP-8 soak report"
        echo
        echo "- Run: \`$(basename "${run_dir}")\`"
        echo "- Collection state: \`${state}\`"
        echo "- Automatic verdict: **${automatic_verdict}**"
        echo "- Target active time: \`$(format_duration "${duration}")\`"
        echo "- Observed active time: \`$(format_duration "${active_seconds}")\`"
        echo "- Samples: ${samples} total / ${live_samples} with a live matching process"
        echo "- Monitored PID: \`$(metadata_value "${run_dir}" pid)\`"
        echo "- Login session: \`$(metadata_value "${run_dir}" session_id)\`"
        echo
        echo "## Resource summary"
        echo
        echo "- RSS: ${first_rss} KiB -> ${last_rss} KiB (change ${rss_growth} KiB)"
        echo "- Peak VmHWM: ${peak_hwm} KiB"
        echo "- File descriptors: ${first_fd} -> ${last_fd} (peak ${peak_fd})"
        echo "- Process CPU ticks: ${first_cpu} -> ${last_cpu}"
        echo "- DRM render-engine time: ${first_render} ns -> ${last_render} ns"
        echo "- Intervals with no DRM render-engine increase: ${idle_intervals}"
        echo "- Filtered kernel DRM failure lines: ${kernel_errors}"
        echo
        echo "## Automatic review"
        echo
        if [[ ${state} == completed ]]; then
            echo "- [x] Required active duration completed without monitored-process loss."
        else
            echo "- [ ] Required active duration did not complete normally."
        fi
        if ((first_rss > 0 && rss_growth > 65536 && rss_growth * 4 > first_rss)); then
            echo "- [!] RSS grew by both more than 64 MiB and more than 25%; inspect workload and samples."
        else
            echo "- [x] RSS did not cross the automatic large-growth threshold."
        fi
        if ((fd_growth > 32)); then
            echo "- [!] File descriptors grew by more than 32; inspect client lifecycle."
        else
            echo "- [x] File-descriptor growth stayed within the automatic threshold."
        fi
        if ((kernel_errors > 0)); then
            echo "- [!] Kernel DRM/GPU failure lines were captured in \`kernel-drm.log\`."
        else
            echo "- [x] No matching kernel DRM/GPU failure line was captured."
        fi
        if ((samples > 1 && idle_intervals == 0)); then
            echo "- [!] No sampled interval showed a flat DRM render counter; confirm an idle period manually."
        else
            echo "- [x] At least one sampled interval had no DRM render-engine increase."
        fi
        echo
        echo "Automatic thresholds are screening aids. Final COMP-8 acceptance requires reviewing"
        echo "the workload, \`samples.csv\`, \`events.log\`, compositor output and this report."
    } >"${report}"
}

collect_run() {
    local run_dir=$1
    local run_id
    local pid expected_start duration interval session_id start_epoch mode
    local active_seconds=0
    local start_wall now last_tick delta wall_seconds
    local next_sample
    local process_alive process_identity current_start
    local current_session_state previous_session_state=unknown
    local current_outputs previous_outputs=""
    local final_state=failed
    local final_reason="collector terminated unexpectedly"
    local termination_requested=0

    run_id=$(basename "${run_dir}")
    pid=$(metadata_value "${run_dir}" pid)
    expected_start=$(metadata_value "${run_dir}" process_start_time)
    duration=$(metadata_value "${run_dir}" duration_seconds)
    interval=$(metadata_value "${run_dir}" interval_seconds)
    session_id=$(metadata_value "${run_dir}" session_id)
    start_epoch=$(metadata_value "${run_dir}" start_epoch)
    mode=$(metadata_value "${run_dir}" mode)

    trap 'termination_requested=1' INT TERM HUP
    trap 'collector_failure_guard "${run_dir}" "$?"' EXIT
    atomic_write "${run_dir}/state" running
    event_log "${run_dir}" collector "started pid=${pid} session=${session_id}"
    start_wall=$(date +%s)
    last_tick=${start_wall}
    next_sample=${start_wall}
    if [[ ${mode} == run ]]; then
        # `run` starts the collector immediately before replacing this shell
        # with the compositor. Avoid recording the short-lived launcher shell
        # as the resource baseline.
        next_sample=$((start_wall + 2))
    fi

    while :; do
        now=$(date +%s)
        delta=$((now - last_tick))
        last_tick=${now}
        wall_seconds=$((now - start_epoch))

        if ((delta > 2)); then
            event_log "${run_dir}" sampling_gap "collector gap ${delta}s; gap not counted as active time"
            delta=0
        fi

        process_alive=no
        process_identity=no
        if [[ -d /proc/${pid} ]]; then
            process_alive=yes
            current_start=$(process_start_time "${pid}" 2>/dev/null || true)
            if [[ ${current_start} == "${expected_start}" ]]; then
                process_identity=yes
            fi
        fi

        current_session_state=no
        if session_is_active "${session_id}"; then
            current_session_state=yes
        fi
        if [[ ${current_session_state} != "${previous_session_state}" ]]; then
            event_log "${run_dir}" session "active=${current_session_state}"
            previous_session_state=${current_session_state}
        fi

        current_outputs=$(output_signature)
        if [[ ${current_outputs} != "${previous_outputs}" ]]; then
            event_log "${run_dir}" outputs "${current_outputs}"
            previous_outputs=${current_outputs}
        fi

        if [[ ${process_alive} == yes && ${process_identity} == yes && ${current_session_state} == yes ]]; then
            active_seconds=$((active_seconds + delta))
            atomic_write "${run_dir}/active_seconds" "${active_seconds}"
        fi

        if ((now >= next_sample)); then
            sample_process "${run_dir}" "${pid}" "${expected_start}" "${active_seconds}" \
                "${wall_seconds}" "${current_session_state}"
            next_sample=$((now + interval))
        fi

        if [[ -f ${run_dir}/stop.request ]]; then
            final_state=stopped
            final_reason="collection stopped by user request"
            break
        fi
        if ((termination_requested)); then
            final_state=stopped
            final_reason="collector received a termination signal"
            break
        fi
        if [[ ${process_alive} != yes ]]; then
            final_state=failed
            final_reason="monitored process exited before completing the target"
            break
        fi
        if [[ ${process_identity} != yes ]]; then
            final_state=failed
            final_reason="monitored PID no longer identifies the original process"
            break
        fi
        if ((active_seconds >= duration)); then
            final_state=completed
            final_reason="target active duration completed"
            break
        fi
        sleep 1
    done

    event_log "${run_dir}" collector "${final_reason}"
    atomic_write "${run_dir}/state" "${final_state}"
    atomic_write "${run_dir}/finished_at" "$(now_iso)"
    write_report "${run_dir}"
    clear_active_run "${run_id}"
    trap - EXIT
}

collector_failure_guard() {
    local run_dir=$1
    local exit_status=$2
    local run_id

    set +e
    if [[ -d ${run_dir} && -f ${run_dir}/state ]] && [[ $(<"${run_dir}/state") == running ]]; then
        run_id=$(basename "${run_dir}")
        event_log "${run_dir}" collector "collector exited unexpectedly with status ${exit_status}"
        atomic_write "${run_dir}/state" failed
        atomic_write "${run_dir}/finished_at" "$(now_iso)"
        write_report "${run_dir}"
        clear_active_run "${run_id}"
    fi
}

ensure_no_active_run() {
    local active_run
    local active_state

    mkdir -p -- "${STATE_ROOT}"
    if [[ ! -f ${STATE_ROOT}/active ]]; then
        return
    fi
    active_run=$(<"${STATE_ROOT}/active")
    if [[ -f ${STATE_ROOT}/${active_run}/state ]]; then
        active_state=$(<"${STATE_ROOT}/${active_run}/state")
        if [[ ${active_state} == starting || ${active_state} == running ]]; then
            die "run ${active_run} is already ${active_state}; stop it before starting another"
        fi
    fi
    rm -f -- "${STATE_ROOT}/active"
}

start_monitor() {
    local pid=$1
    local duration=$2
    local interval=$3
    local session_id=$4
    local mode=$5
    local run_id run_dir unit expected_start command_line start_epoch

    [[ ${pid} =~ ^[1-9][0-9]*$ ]] || die "PID must be a positive integer"
    [[ -d /proc/${pid} ]] || die "process ${pid} does not exist"
    loginctl show-session "${session_id}" >/dev/null 2>&1 \
        || die "logind session '${session_id}' does not exist"
    command -v systemd-run >/dev/null || die "systemd-run is required"
    command -v systemd-inhibit >/dev/null || die "systemd-inhibit is required"
    ensure_no_active_run

    expected_start=$(process_start_time "${pid}") \
        || die "cannot read process identity for PID ${pid}"
    command_line=$(process_command "${pid}")
    start_epoch=$(date +%s)
    run_id="$(date -u +%Y%m%dT%H%M%SZ)-${pid}"
    run_dir="${STATE_ROOT}/${run_id}"
    unit="nkdhr-soak-${run_id}"
    [[ ! -e ${run_dir} ]] || die "run directory already exists: ${run_dir}"
    mkdir -p -- "${run_dir}"

    {
        echo "run_id=${run_id}"
        echo "mode=${mode}"
        echo "pid=${pid}"
        echo "process_start_time=${expected_start}"
        echo "process_command=${command_line}"
        echo "session_id=${session_id}"
        echo "session_tty=$(loginctl show-session "${session_id}" -p TTY --value 2>/dev/null || true)"
        echo "duration_seconds=${duration}"
        echo "interval_seconds=${interval}"
        echo "start_epoch=${start_epoch}"
        echo "started_at=$(now_iso)"
        echo "unit=${unit}.service"
        echo "host=$(hostname)"
        echo "kernel=$(uname -r)"
        echo "clock_ticks=$(getconf CLK_TCK)"
        echo "repo_commit=$(git -C "${REPO_ROOT}" rev-parse HEAD 2>/dev/null || echo unknown)"
    } >"${run_dir}/metadata.txt"
    printf '%s\n' starting >"${run_dir}/state"
    printf '0\n' >"${run_dir}/active_seconds"
    : >"${run_dir}/events.log"
    : >"${run_dir}/compositor.log"
    : >"${run_dir}/kernel-drm.log"
    printf '%s\n' \
        'timestamp,wall_seconds,active_seconds,session_active,pid_alive,pid_identity,rss_kib,vmhwm_kib,threads,fd_count,cpu_ticks,drm_clients,drm_render_ns,drm_copy_ns,drm_video_ns,drm_video_enhance_ns,drm_total_kib,drm_resident_kib,drm_available,outputs' \
        >"${run_dir}/samples.csv"
    atomic_write "${STATE_ROOT}/latest" "${run_id}"
    atomic_write "${STATE_ROOT}/active" "${run_id}"

    if ! systemd-run --user --quiet --collect --unit="${unit}" \
        --description="nkdhr COMP-8 soak ${run_id}" --property=Type=exec \
        systemd-inhibit --what=sleep --who=nkdhr-soak \
        --why="nkdhr COMP-8 stability measurement" --mode=block \
        "${SCRIPT_PATH}" _collect "${run_dir}"; then
        atomic_write "${run_dir}/state" failed
        clear_active_run "${run_id}"
        die "failed to start the transient collector unit"
    fi

    for _ in {1..50}; do
        if [[ $(<"${run_dir}/state") == running ]]; then
            break
        fi
        sleep 0.1
    done
    [[ $(<"${run_dir}/state") == running ]] \
        || die "collector did not enter running state; inspect systemctl --user status ${unit}.service"
    printf '%s\n' "${run_dir}"
}

parse_start_options() {
    local mode=$1
    shift
    local duration=8h
    local interval=30s
    local session_id=""
    local pid=""
    local duration_seconds interval_seconds run_dir
    local -a command=()

    while (($#)); do
        case $1 in
            --duration)
                (($# >= 2)) || die "--duration requires a value"
                duration=$2
                shift 2
                ;;
            --interval)
                (($# >= 2)) || die "--interval requires a value"
                interval=$2
                shift 2
                ;;
            --session)
                (($# >= 2)) || die "--session requires a logind session id"
                session_id=$2
                shift 2
                ;;
            --pid)
                [[ ${mode} == attach ]] || die "--pid is valid only with start"
                (($# >= 2)) || die "--pid requires a value"
                pid=$2
                shift 2
                ;;
            --)
                shift
                command=("$@")
                break
                ;;
            -*) die "unknown option '$1'" ;;
            *)
                [[ ${mode} == run ]] || die "unexpected argument '$1'"
                command=("$@")
                break
                ;;
        esac
    done

    duration_seconds=$(parse_duration "${duration}")
    interval_seconds=$(parse_duration "${interval}")
    ((interval_seconds <= duration_seconds)) \
        || die "sample interval must not exceed the target duration"
    if [[ -z ${session_id} ]]; then
        session_id=$(detect_session) \
            || die "cannot detect a local login session; pass --session ID"
    fi

    if [[ ${mode} == attach ]]; then
        [[ -n ${pid} ]] || die "start requires --pid PID"
        run_dir=$(start_monitor "${pid}" "${duration_seconds}" "${interval_seconds}" "${session_id}" attach)
        echo "${SCRIPT_NAME}: collector started for PID ${pid}"
        echo "${SCRIPT_NAME}: run directory ${run_dir}"
        return
    fi

    if [[ ${#command[@]} -eq 0 ]]; then
        command=("${REPO_ROOT}/target/release/nkdhr-canvas" --tty)
    fi
    [[ -x ${command[0]} ]] || die "command is not executable: ${command[0]}"
    run_dir=$(start_monitor "$$" "${duration_seconds}" "${interval_seconds}" "${session_id}" run)
    echo "${SCRIPT_NAME}: collector started; run directory ${run_dir}"
    echo "${SCRIPT_NAME}: exec ${command[*]}"
    exec > >(tee -a "${run_dir}/compositor.log") 2>&1
    exec "${command[@]}"
}

show_status() {
    local run_dir=$1
    local state active duration pid unit pid_state=absent

    state=$(<"${run_dir}/state")
    active=$(<"${run_dir}/active_seconds")
    duration=$(metadata_value "${run_dir}" duration_seconds)
    pid=$(metadata_value "${run_dir}" pid)
    unit=$(metadata_value "${run_dir}" unit)
    if [[ -d /proc/${pid} ]] && [[ $(process_start_time "${pid}" 2>/dev/null || true) == "$(metadata_value "${run_dir}" process_start_time)" ]]; then
        pid_state=alive
    fi
    echo "run: $(basename "${run_dir}")"
    echo "state: ${state}"
    echo "active: $(format_duration "${active}") / $(format_duration "${duration}")"
    echo "process: ${pid_state} (PID ${pid})"
    echo "collector unit: ${unit}"
    echo "directory: ${run_dir}"
    if [[ -f ${run_dir}/report.md ]]; then
        echo "report: ${run_dir}/report.md"
    fi
}

stop_run() {
    local run_dir=$1
    local state

    state=$(<"${run_dir}/state")
    if [[ ${state} != running && ${state} != starting ]]; then
        echo "${SCRIPT_NAME}: run $(basename "${run_dir}") is already ${state}"
        return
    fi
    : >"${run_dir}/stop.request"
    for _ in {1..30}; do
        state=$(<"${run_dir}/state")
        if [[ ${state} != running && ${state} != starting ]]; then
            break
        fi
        sleep 0.1
    done
    echo "${SCRIPT_NAME}: collection state is ${state}; the monitored compositor was not terminated"
    echo "${SCRIPT_NAME}: run directory ${run_dir}"
}

self_test() {
    local fixture_pid run_dir state

    echo "${SCRIPT_NAME}: starting a short detached collector self-test"
    sleep 15 &
    fixture_pid=$!
    run_dir=$(start_monitor "${fixture_pid}" 3 1 "$(detect_session)" self-test)
    for _ in {1..60}; do
        state=$(<"${run_dir}/state")
        if [[ ${state} != running && ${state} != starting ]]; then
            break
        fi
        sleep 0.1
    done
    kill -TERM "${fixture_pid}" 2>/dev/null || true
    wait "${fixture_pid}" 2>/dev/null || true
    state=$(<"${run_dir}/state")
    [[ ${state} == completed ]] || die "self-test ended in state ${state}; inspect ${run_dir}"
    [[ -s ${run_dir}/report.md ]] || die "self-test did not produce a report"
    echo "${SCRIPT_NAME}: self-test passed"
    show_status "${run_dir}"
}

case ${1:-} in
    run)
        shift
        parse_start_options run "$@"
        ;;
    start)
        shift
        parse_start_options attach "$@"
        ;;
    status)
        (($# <= 2)) || die "status accepts at most one run id"
        show_status "$(resolve_run_dir "${2:-}")"
        ;;
    stop)
        (($# <= 2)) || die "stop accepts at most one run id"
        stop_run "$(resolve_run_dir "${2:-}")"
        ;;
    report)
        (($# <= 2)) || die "report accepts at most one run id"
        run_dir=$(resolve_run_dir "${2:-}")
        write_report "${run_dir}"
        cat "${run_dir}/report.md"
        ;;
    self-test)
        (($# == 1)) || die "self-test takes no arguments"
        self_test
        ;;
    _collect)
        (($# == 2)) || die "internal collector requires one run directory"
        collect_run "$2"
        ;;
    -h | --help | help | "")
        usage
        ;;
    *)
        usage >&2
        exit 2
        ;;
esac
