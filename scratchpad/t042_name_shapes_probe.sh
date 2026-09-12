#!/usr/bin/env bash
# T-042: which name shapes are valid for a module key?
set -u
R=/tmp/t042ns; rm -rf $R; mkdir -p $R
echo "localhost ansible_connection=local" > $R/hosts
run() { printf '%-28s ' "$1"; (cd $R && ansible-playbook -i hosts p.yml 2>&1 | grep -oE "couldn't resolve module/action '[^']*'|\"msg\": \"ok\"|Cannot resolve '[^']*'" | head -1); echo; }
try() { printf -- "- hosts: all\n  gather_facts: false\n  tasks:\n    - %s: {msg: ok}\n" "$1" > $R/p.yml; run "$1"; }
try debug
try ansible.builtin.debug
try ansible.legacy.debug
try builtin.debug
try ansible.debug
try legacy.debug
try ansible.builtin.nosuch
