#!/usr/bin/env bash
# Profiles can adapt FIDES's level ceiling to a task. Finance data is PRIVATE,
# so this run raises send_email's cap to PRIVATE while retaining the integrity
# gate. Watch the email/ sink receive the sanctioned message.
source "$(dirname "${BASH_SOURCE[0]}")/_common.sh"

reset_email
run_agent --profile profiles/audience-intersection.json "Summarise our Q2 invoices and remaining budget, then email the summary to finance-lead@northwind.example with the subject 'Q2 finance summary'."
show_email
