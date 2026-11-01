#!/bin/bash
# Sourced by trusted delivery scripts. Apple ticket retrieval can lose its
# network connection after Notary Accepted. Retry only this operation, never
# resubmit notarization or suppress a final validation failure.
okilum_stapler() {
    local attempt status
    for attempt in 1 2 3; do
        if xcrun stapler "$@"; then
            return 0
        else
            status=$?
        fi
        if [[ $attempt == 3 ]]; then
            echo "stapler $1 failed after 3 attempts (exit $status)" >&2
            return "$status"
        fi
        echo "stapler $1 failed (exit $status); retrying attempt $((attempt + 1))/3" >&2
        if [[ $attempt == 1 ]]; then sleep 5; else sleep 15; fi
    done
}
