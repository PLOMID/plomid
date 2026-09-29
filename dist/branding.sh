#!/bin/bash
# 
# © 2026 PLOMID Technology Solutions
#
# PLOMID
# Platform for Modern Intelligence and Data
#
# Author: Sainath Sapa
# GitHub: https://github.com/sainathsapa
#
# Licensed under the Apache License, Version 2.0;
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#     https://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.
# PLOMID brand + log helpers for packaging scripts.
# Source it:  . "$(dirname "$0")/../branding.sh"
# Colors follow the brand sheet. Ink/graphite/paper are documented for
# completeness; scripts only paint the copper accent so output stays
# readable on dark and light terminals alike.
#
#   Copper --accent   #e2683c  live parts, emphasis, section banners
#   Ink    --ink      #08090b  deepest ground (dark sections)
#   Graphite --graphite #14171b dark section tone
#   Paper  --paper    #f3f1ec  warm light ground

PLOMID_COPPER=$'\033[38;2;226;104;60m'
PLOMID_BOLD=$'\033[1m'
PLOMID_DIM=$'\033[2m'
PLOMID_RESET=$'\033[0m'

# section "label": copper banner line for release/build logs.
section() {
    printf '%s==>%s %s%s%s\n' \
        "$PLOMID_COPPER" "$PLOMID_RESET" \
        "$PLOMID_BOLD" "$1" "$PLOMID_RESET"
}

# ok "label": copper check-mark line for completed artifacts.
ok() {
    printf '%s✔%s %s\n' "$PLOMID_COPPER" "$PLOMID_RESET" "$1"
}

# warn "label": plain warning line (no red; release logs stay calm).
warn() {
    printf '%s!!%s %s\n' "$PLOMID_DIM" "$PLOMID_RESET" "$1"
}
