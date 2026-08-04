#!/usr/bin/env bash

set -euo pipefail

readonly CONFIGFS_MOUNT="/sys/kernel/config"
readonly VKMS_ROOT="${CONFIGFS_MOUNT}/vkms"
readonly INSTANCE_NAME="nkdhr-lab"
readonly INSTANCE_ROOT="${VKMS_ROOT}/${INSTANCE_NAME}"

usage() {
    cat <<'EOF'
Usage: vkms-lab.sh setup
       vkms-lab.sh show
       vkms-lab.sh audit <nkdhr-canvas-pid>
       vkms-lab.sh connect <0|1>
       vkms-lab.sh disconnect <0|1>
       vkms-lab.sh teardown

Creates one VKMS device with two independent display pipelines for
nkdhr-canvas multi-output and hotplug verification.
EOF
}

die() {
    echo "vkms-lab: $*" >&2
    exit 1
}

require_root() {
    [[ ${EUID} -eq 0 ]] || die "this command mutates configfs and must run as root"
}

ensure_configfs() {
    if ! mountpoint -q "${CONFIGFS_MOUNT}"; then
        mount -t configfs none "${CONFIGFS_MOUNT}"
    fi
}

ensure_vkms() {
    ensure_configfs
    if [[ ! -d /sys/module/vkms ]]; then
        modprobe vkms create_default_dev=0
    fi
    [[ -d ${VKMS_ROOT} ]] || die "the loaded VKMS module does not expose configfs"
}

unlink_if_present() {
    local path=$1
    if [[ -L ${path} ]]; then
        unlink "${path}"
    fi
}

rmdir_if_present() {
    local path=$1
    if [[ -d ${path} ]]; then
        rmdir "${path}"
    fi
}

remove_instance() {
    local index

    if [[ -f ${INSTANCE_ROOT}/enabled ]]; then
        printf '0\n' >"${INSTANCE_ROOT}/enabled"
    fi

    for index in 0 1; do
        unlink_if_present "${INSTANCE_ROOT}/planes/plane${index}/possible_crtcs/crtc${index}"
        unlink_if_present "${INSTANCE_ROOT}/encoders/encoder${index}/possible_crtcs/crtc${index}"
        unlink_if_present "${INSTANCE_ROOT}/connectors/connector${index}/possible_encoders/encoder${index}"
    done

    for index in 0 1; do
        rmdir_if_present "${INSTANCE_ROOT}/connectors/connector${index}"
        rmdir_if_present "${INSTANCE_ROOT}/encoders/encoder${index}"
        rmdir_if_present "${INSTANCE_ROOT}/crtcs/crtc${index}"
        rmdir_if_present "${INSTANCE_ROOT}/planes/plane${index}"
    done
    rmdir_if_present "${INSTANCE_ROOT}"
}

rollback_setup() {
    local status=$?
    trap - ERR INT TERM
    set +e
    remove_instance
    echo "vkms-lab: setup failed; removed the partial ${INSTANCE_NAME} instance" >&2
    exit "${status}"
}

setup() {
    local index

    require_root
    ensure_vkms
    [[ ! -e ${INSTANCE_ROOT} ]] || die "${INSTANCE_ROOT} already exists; refusing to overwrite it"

    mkdir "${INSTANCE_ROOT}"
    trap rollback_setup ERR INT TERM

    for index in 0 1; do
        mkdir "${INSTANCE_ROOT}/planes/plane${index}"
        mkdir "${INSTANCE_ROOT}/crtcs/crtc${index}"
        mkdir "${INSTANCE_ROOT}/encoders/encoder${index}"
        mkdir "${INSTANCE_ROOT}/connectors/connector${index}"

        printf '1\n' >"${INSTANCE_ROOT}/planes/plane${index}/type"
        printf '1\n' >"${INSTANCE_ROOT}/connectors/connector${index}/status"

        ln -s "${INSTANCE_ROOT}/crtcs/crtc${index}" \
            "${INSTANCE_ROOT}/planes/plane${index}/possible_crtcs"
        ln -s "${INSTANCE_ROOT}/crtcs/crtc${index}" \
            "${INSTANCE_ROOT}/encoders/encoder${index}/possible_crtcs"
        ln -s "${INSTANCE_ROOT}/encoders/encoder${index}" \
            "${INSTANCE_ROOT}/connectors/connector${index}/possible_encoders"
    done

    printf '1\n' >"${INSTANCE_ROOT}/enabled"
    udevadm settle --timeout=5 || true
    trap - ERR INT TERM

    echo "vkms-lab: created ${INSTANCE_NAME} with two connected outputs"
    show
}

set_connector_status() {
    local index=$1
    local status=$2
    local action=$3

    require_root
    case "${index}" in
        0 | 1) ;;
        *) die "connector index must be 0 or 1" ;;
    esac
    [[ -f ${INSTANCE_ROOT}/enabled ]] || die "${INSTANCE_NAME} is not configured; run setup first"
    [[ $(<"${INSTANCE_ROOT}/enabled") == 1 ]] || die "${INSTANCE_NAME} is not enabled"

    printf '%s\n' "${status}" >"${INSTANCE_ROOT}/connectors/connector${index}/status"
    udevadm settle --timeout=5 || true
    echo "vkms-lab: connector${index} ${action}"
    show
}

show() {
    local connector
    local found=0
    local sysfs_name

    if [[ -f ${INSTANCE_ROOT}/enabled ]]; then
        echo "vkms-lab: ${INSTANCE_NAME} enabled=$(<"${INSTANCE_ROOT}/enabled")"
        for connector in 0 1; do
            echo "  config connector${connector} status=$(<"${INSTANCE_ROOT}/connectors/connector${connector}/status")"
        done
    else
        echo "vkms-lab: ${INSTANCE_NAME} is not configured"
    fi

    for connector in /sys/class/drm/card*-Virtual-*; do
        [[ -e ${connector} ]] || continue
        found=1
        sysfs_name=$(basename "${connector}")
        echo "  drm ${sysfs_name} status=$(<"${connector}/status")"
        echo "  scanout device: /dev/dri/${sysfs_name%%-*}"
    done
    if [[ ${found} -eq 0 ]]; then
        echo "  no VKMS Virtual connectors are currently exposed"
    fi
}

audit_process() {
    local pid=$1
    local fd
    local target
    local connector
    local sysfs_name
    local found_render=0
    local found_scanout=0
    declare -A allowed_primary=()

    [[ ${pid} =~ ^[1-9][0-9]*$ ]] || die "PID must be a positive integer"
    [[ -d /proc/${pid}/fd ]] || die "process ${pid} does not exist or its descriptors are inaccessible"

    for connector in /sys/class/drm/card*-Virtual-*; do
        [[ -e ${connector} ]] || continue
        sysfs_name=$(basename "${connector}")
        allowed_primary["/dev/dri/${sysfs_name%%-*}"]=1
    done
    [[ ${#allowed_primary[@]} -gt 0 ]] || die "no VKMS Virtual connector is exposed"

    echo "vkms-lab: DRM descriptors for process ${pid}:"
    for fd in /proc/"${pid}"/fd/*; do
        target=$(readlink "${fd}") || continue
        case ${target} in
            /dev/dri/renderD*)
                found_render=1
                echo "  ${target} (render)"
                ;;
            /dev/dri/card*)
                if [[ -z ${allowed_primary[${target}]:-} ]]; then
                    die "unsafe excluded primary node is open: ${target}"
                fi
                found_scanout=1
                echo "  ${target} (VKMS scanout)"
                ;;
        esac
    done

    [[ ${found_render} -eq 1 ]] || die "the process has no DRM render node open"
    [[ ${found_scanout} -eq 1 ]] || die "the process has no VKMS primary node open"
    echo "vkms-lab: audit passed; no excluded primary DRM node is open"
}

teardown() {
    require_root
    if [[ ! -d ${INSTANCE_ROOT} ]]; then
        echo "vkms-lab: ${INSTANCE_NAME} is already absent"
        return
    fi
    remove_instance
    udevadm settle --timeout=5 || true
    echo "vkms-lab: removed ${INSTANCE_NAME}; the vkms module remains loaded"
}

case ${1:-} in
    setup)
        [[ $# -eq 1 ]] || die "setup takes no arguments"
        setup
        ;;
    show)
        [[ $# -eq 1 ]] || die "show takes no arguments"
        show
        ;;
    audit)
        [[ $# -eq 2 ]] || die "audit requires an nkdhr-canvas PID"
        audit_process "$2"
        ;;
    connect)
        [[ $# -eq 2 ]] || die "connect requires connector index 0 or 1"
        set_connector_status "$2" 1 connected
        ;;
    disconnect)
        [[ $# -eq 2 ]] || die "disconnect requires connector index 0 or 1"
        set_connector_status "$2" 2 disconnected
        ;;
    teardown)
        [[ $# -eq 1 ]] || die "teardown takes no arguments"
        teardown
        ;;
    *)
        usage >&2
        exit 2
        ;;
esac
