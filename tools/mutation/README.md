# Deliberate-bug checks

Each case breaks one safety rule in the source, runs the one test that should notice, and puts the
source back. A case is **caught** only if the mutated code compiled and the test run failed. A mutant
that does not compile counts as not caught, so every survivor is either a test gap (fix the test) or a
bad mutant (fix the mutant). Cargo runs serially, targeted with `-p`, with `-j 8`.

| Script | Groups |
|---|---|
| `mutation_check.py` | `u1 u3 u4 p4 p10 v1 v2 v3 sd lc lr nr ct u2` (connections, model sources, node packs, media, client flows, declarative adapter). Cases for `u2` are in `mutation_u2.py`. |
| `mutation_check.py ps` | Node pack status report (`pack/status.rs`): links, hidden folders, names, the install record. |
| `mutation_check.py lg` | Remembered licences (`registry/licences.rs`, `install.rs`): terms, versions, local files, damaged record. |
| `sam3_mutation_check.py` | `sg1 sg2 sg2b sg2c sg3` (SAM 3 masks, sessions, slots, cancel). |

```sh
python tools/mutation/mutation_check.py u2            # one group
python tools/mutation/mutation_check.py u2 stale      # cases whose name contains "stale"
```

Group ids are historical labels. The header of each test file says what it covers.

Run them exclusively. They snapshot and restore source files, so do not edit, format or build those
files while one runs. Afterwards read the restored source and rerun the focused tests.

Logs go to `tools/mutation/verification/` (not committed) after passing through `scrub.py`, which
replaces a Windows or Unix profile folder with `%USERPROFILE%` so no account name is written.
