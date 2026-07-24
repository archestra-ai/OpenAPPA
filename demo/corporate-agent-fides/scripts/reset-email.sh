#!/usr/bin/env bash
# Clear the send_email sink (data/email/).
source "$(dirname "${BASH_SOURCE[0]}")/_common.sh"
reset_email
echo "cleared data/email/"
