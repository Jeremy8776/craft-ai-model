"""Exercise sg1 safety properties by deliberate mutants, restoring source after each.
Usage: mutation_check.py [sg1|sg2|sg2b|sg2c|sg3]   (default sg1). Only this slice's files are touched. Cargo runs serially, targeted, with -j 8.
"""
import json
import os
import re
import sys
import subprocess
from scrub import scrub
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
OUT = Path(__file__).resolve().parent / 'verification'
OUT.mkdir(exist_ok=True)
MASKS = ROOT / 'crates/ai-comfyui/src/sam3_masks.rs'
SESSION = ROOT / 'crates/ai-comfyui/src/sam3_session.rs'
DETECT = ROOT / 'crates/ai-comfyui/src/sam3_detect.rs'
INSTANCES = ROOT / 'crates/ai-comfyui/src/sam3_session/instances.rs'
CAND_TYPES = ROOT / 'crates/ai-comfyui/src/sam3_candidates.rs'
CAND_SESSION = ROOT / 'crates/ai-comfyui/src/sam3_session/candidates.rs'
LEASE = ROOT / 'crates/ai-comfyui/src/sam3_lease.rs'
QUEUE = ROOT / 'crates/ai-comfyui/src/sam3_session/queue.rs'
GROUP = sys.argv[1] if len(sys.argv) > 1 else 'sg1'
CASES = [
    ('first-mask-only', MASKS, 'for file in files {\n        if remaining == 0',
     'for file in files.iter().take(1) {\n        if remaining == 0', 'sam3_masks', 'grounding_unions_every_instance'),
    ('mask-count-loosened', MASKS, 'const MAX_MASKS: usize = 100;',
     'const MAX_MASKS: usize = 101;', 'sam3_mask_limits', 'more_than_one_hundred'),
    ('decoded-pixel-limit-loosened', MASKS, 'const MAX_DECODED_PIXELS: u64 = 64 * 1024 * 1024;',
     'const MAX_DECODED_PIXELS: u64 = 128 * 1024 * 1024;', 'sam3_mask_limits', 'aggregate_decoded_pixel_budget'),
    ('encoded-byte-limit-loosened', MASKS, 'const MAX_ENCODED_BYTES: u64 = 32 * 1024 * 1024;',
     'const MAX_ENCODED_BYTES: u64 = 64 * 1024 * 1024;', 'sam3_mask_limits', 'aggregate_encoded_byte_budget'),
    ('wrong-union-operation', MASKS, '*out = (*out).max(*value);',
     '*out = (*out).min(*value);', 'sam3_mask_limits', 'union_preserves_soft_coverage'),
    ('dimension-check-disabled', MASKS, 'if reader.info().width != width || reader.info().height != height {',
     'if false {', 'sam3_mask_limits', 'a_wrong_sized_later_mask'),
    ('output-type-preflight-disabled', MASKS, 'if file.get("type").and_then(Value::as_str) != Some("output")',
     'if false', 'sam3_mask_limits', 'non_output_later_descriptor'),
    ('output-sink-marker-reused', QUEUE, 'NEXT_OUTPUT.fetch_add(1, Ordering::Relaxed)',
     'NEXT_OUTPUT.fetch_add(0, Ordering::Relaxed)', 'sam3_cached', 'repeated_grounding'),
]
SG2 = [
    ('stale-stamp-accepted', INSTANCES, "fn unstamped<'a>(text: &'a str, stamp: &str) -> Result<&'a str, AiError> {",
     "fn unstamped<'a>(text: &'a str, stamp: &str) -> Result<&'a str, AiError> {"
     "\n    if let Some((_, rest)) = text.split_once('\\n') {\n        return Ok(rest);\n    }",
     'sam3_instances', 'text_stamped_for_another_request'),
    ('stamp-constant', INSTANCES, 'fn stamp() -> String {', 'fn stamp() -> String {\n    return "craft-constant".to_owned();', 'sam3_instances', 'a_repeat_changes_only_the_stamp'),
    ('preview-gate-disabled', INSTANCES, 'if !prepared.preview_text {', 'if false {', 'sam3_instances', 'a_server_without_preview_any'),
    ('no-match-check-disabled', INSTANCES, 'if metadata.is_empty() {', 'if false {', 'sam3_instances', 'zero_detections'),
    ('count-check-disabled', DETECT, 'if masks.len() != self.0.len() {', 'if false {', 'sam3_instances', 'consistent_boxes_and_scores'),
    ('box-outside-image-accepted', DETECT, ' || x2 > w || y2 > h {', ' {', 'sam3_instances', 'malformed_or_impossible'),
    ('score-range-loosened', DETECT, '(0.0..=1.0).contains(v)', '(0.0..=2.0).contains(v)', 'sam3_instances', 'malformed_or_impossible'),
    ('text-limit-loosened', DETECT, 'const MAX_TEXT_BYTES: usize = 64 * 1024;', 'const MAX_TEXT_BYTES: usize = 1024 * 1024;', 'sam3_instances', 'oversized_metadata'),
    ('pick-lowest-score', DETECT, '.max_by(|a, b|', '.min_by(|a, b|', 'sam3_instances', 'picking_chooses_the_highest'),
    ('pick-coverage-loosened', DETECT, 'const PICK_COVERAGE: u8 = 128;', 'const PICK_COVERAGE: u8 = 1;', 'sam3_instances', 'picking_ignores_pixels'),
    ('mask-bounds-off-by-one', DETECT, 'x2 = x2.max(x + 1);', 'x2 = x2.max(x);', 'sam3_instances', 'instances_keep_mask_box_score'),
]
SG2B = [
    ('candidate-stale-stamp-accepted', INSTANCES, "fn unstamped<'a>(text: &'a str, stamp: &str) -> Result<&'a str, AiError> {",
     "fn unstamped<'a>(text: &'a str, stamp: &str) -> Result<&'a str, AiError> {"
     "\n    if let Some((_, rest)) = text.split_once('\\n') {\n        return Ok(rest);\n    }",
     'sam3_candidates', 'text_not_stamped'),
    ('candidate-stamp-constant', INSTANCES, 'pub(super) fn stamp() -> String {',
     'pub(super) fn stamp() -> String {\n    return "craft-constant".to_owned();', 'sam3_candidates', 'a_repeat_changes_only'),
    ('score-count-check-disabled', CAND_TYPES, 'if masks.len() != scores.len() {', 'if false {', 'sam3_candidates', 'a_score_count_that_differs'),
    ('all-empty-accepted', CAND_TYPES, 'if candidates.iter().all(|c| c.mask_bounds.is_none()) {', 'if false {', 'sam3_candidates', 'an_empty_candidate'),
    ('candidate-limit-loosened', CAND_TYPES, 'pub(crate) const MAX_CANDIDATES: usize = 3;', 'pub(crate) const MAX_CANDIDATES: usize = 4;', 'sam3_candidates', 'more_than_three'),
    ('candidate-limit-check-disabled', CAND_TYPES, 'if items.len() > MAX_CANDIDATES {', 'if false {', 'sam3_candidates', 'more_than_three'),
    ('candidate-order-reversed', CAND_TYPES, 'masks.into_iter().zip(scores).enumerate()', 'masks.into_iter().rev().zip(scores).enumerate()', 'sam3_candidates', 'candidates_keep_server_order'),
    ('highest-scoring-picks-lowest', CAND_TYPES, '.max_by(|a, b|', '.min_by(|a, b|', 'sam3_candidates', 'candidates_keep_server_order'),
    ('point-limit-loosened', CAND_SESSION, 'const MAX_POINTS: usize = 32;', 'const MAX_POINTS: usize = 64;', 'sam3_candidates', 'unsupported_or_invalid'),
    ('segmentation-gate-removed', CAND_SESSION, 'if !prepared.segmentation {', 'if false {', 'sam3_candidates', 'a_server_missing'),
    ('positive-points-sent-as-the-wrong-list', CAND_SESSION, '"coordinates": coordinates(&prompt.positive_points),', '"coordinates": coordinates(&prompt.negative_points),', 'sam3_candidates', 'the_graph_sends_points'),
    ('multimask-off', CAND_SESSION, '"use_multimask": true', '"use_multimask": false', 'sam3_candidates', 'the_graph_sends_points'),
    ('best-mask-only', CAND_SESSION, '"output_best_mask": false', '"output_best_mask": true', 'sam3_candidates', 'the_graph_sends_points'),
]
SG2C = [
    ('box-centre-x-not-halved', CAND_SESSION, '(ax + bx) / 2.0 / w,', '(ax + bx) / w,', 'sam3_candidate_inputs', 'a_box_is_sent'),
    ('box-centre-y-not-halved', CAND_SESSION, '(ay + by) / 2.0 / h,', '(ay + by) / h,', 'sam3_candidate_inputs', 'a_box_is_sent'),
    ('box-width-uses-height', CAND_SESSION, '(ax - bx).abs() / w,', '(ax - bx).abs() / h,', 'sam3_candidate_inputs', 'a_box_is_sent'),
    ('box-height-uses-width', CAND_SESSION, '(ay - by).abs() / h,', '(ay - by).abs() / w,', 'sam3_candidate_inputs', 'a_box_is_sent'),
    ('negative-width-for-reversed-x', CAND_SESSION, '(ax - bx).abs() / w,', '(ax - bx) / w,', 'sam3_candidate_inputs', 'any_corner_order'),
    ('negative-height-for-reversed-y', CAND_SESSION, '(ay - by).abs() / h,', '(ay - by) / h,', 'sam3_candidate_inputs', 'any_corner_order'),
    ('box-marked-negative', CAND_SESSION, '"is_positive": true', '"is_positive": false', 'sam3_candidate_inputs', 'a_box_is_sent'),
    ('box-not-wired-into-segmentation', CAND_SESSION, 'graph["3"]["inputs"]["box"] = json!(["12", 0]);', 'let _unused = 0;', 'sam3_candidate_inputs', 'a_box_is_sent'),
    ('box-node-gate-removed', CAND_SESSION, 'if prompt.box_.is_some() && !prepared.boxes {', 'if false {', 'sam3_candidate_inputs', 'its_nodes_are_missing'),
    ('box-nodes-required-for-points', CAND_SESSION, 'if prompt.box_.is_some() && !prepared.boxes {', 'if !prepared.boxes {', 'sam3_candidate_inputs', 'needs_none_installed'),
    ('refinement-limit-loosened', CAND_SESSION, 'const MAX_REFINEMENT: u8 = 10;', 'const MAX_REFINEMENT: u8 = 11;', 'sam3_candidate_inputs', 'the_refinement_count'),
    ('refinement-check-disabled', CAND_SESSION, 'if refinement_passes > MAX_REFINEMENT {', 'if false {', 'sam3_candidate_inputs', 'the_refinement_count'),
    ('refinement-not-sent', CAND_SESSION, '"refinement_iterations": refinement,', '"refinement_iterations": 0,', 'sam3_candidate_inputs', 'the_refinement_count'),
]
SG3 = [
    ('slot-node-id-shared', LEASE, '        format!("craft_ai_sam3_{}", self.slot)', '        "craft_ai_sam3_0".to_owned()', 'sam3_leases', 'two_sessions_on_one_server'),
    ('slot-lock-ignored', LEASE, 'Err(TryLockError::WouldBlock) => continue,', 'Err(TryLockError::WouldBlock) => return Ok(Self { _file: file, slot }),', 'sam3_leases', 'two_sessions_on_one_server'),
    ('slot-limit-loosened', LEASE, 'pub(crate) const SLOTS: usize = 4;', 'pub(crate) const SLOTS: usize = 5;', 'sam3_leases', 'slots_are_bounded'),
    ('slot-pool-not-per-server', LEASE, 'dir.join(format!("slot-{server}-{slot}.lock"))', 'dir.join(format!("slot-{slot}.lock"))', 'sam3_leases', 'each_server_has_its_own_pool'),
    ('release-keeps-the-slot', SESSION, 'self.lease = None;', 'let _keep = &self.lease;', 'sam3_leases', 'release_gives_the_slot_back'),
    ('prepare-takes-a-new-slot-every-time', SESSION, 'if self.lease.is_none() {', 'if true {', 'sam3_leases', 'preparing_again_keeps'),
    ('prepare-does-not-start-a-revision', SESSION, '        // Anything computed before this call is now stale, whether or not the call succeeds.\n        self.revision += 1;', '        let _bump = 0;', 'sam3_lifecycle', 'each_prepare_numbers'),
    ('release-does-not-start-a-revision', SESSION, '    pub fn release(&mut self) {\n        self.revision += 1;', '    pub fn release(&mut self) {', 'sam3_lifecycle', 'a_failed_prepare_or_a_release'),
    ('current-ignores-whether-prepared', SESSION, 'self.prepared.is_some() && revision == self.revision()', 'revision == self.revision()', 'sam3_lifecycle', 'nothing_is_current'),
    ('candidates-not-stamped', CAND_SESSION, '.map(|found| found.stamped(self.revision()))', '.map(|found| found)', 'sam3_lifecycle', 'each_prepare_numbers'),
    ('instances-not-stamped', INSTANCES, '.map(|found| found.stamped(self.revision()))', '.map(|found| found)', 'sam3_lifecycle', 'grounded_instances_carry'),
    ('cancellation-ignored', QUEUE, 'if self.canceller.fired_since(started) {', 'if false {', 'sam3_lifecycle', 'cancelling_stops_the_wait'),
    ('cancel-clears-the-whole-queue', QUEUE, 'json!({"delete": [id]})', 'json!({"clear": true})', 'sam3_lifecycle', 'cancelling_stops_the_wait'),
    ('cancel-leaves-its-prompt-queued', QUEUE, 'let _ = self.http.post("/queue", &json!({"delete": [id]}));', 'let _ = ();', 'sam3_lifecycle', 'cancelling_stops_the_wait'),
    ('interrupt-sent-without-confirmation', QUEUE, 'if self.targeted_interrupt {', 'if true {', 'sam3_lifecycle', 'cancelling_stops_the_wait'),
    ('interrupt-names-no-prompt', QUEUE, 'json!({"prompt_id": id})', 'json!({})', 'sam3_lifecycle', 'with_a_confirmed_server'),
]
TIMEOUTS = {'cancellation-ignored': 45}
if GROUP == 'sg3':
    CASES = SG3
elif GROUP == 'sg2':
    CASES = SG2
elif GROUP == 'sg2c':
    CASES = SG2C
elif GROUP == 'sg2b':
    CASES = SG2B
elif GROUP != 'sg1':
    raise SystemExit('group must be sg1, sg2, sg2b, sg2c or sg3')
results = []
for name, path, old, new, target, test in CASES:
    original = path.read_bytes()
    text = original.decode('utf-8').replace('\r\n', '\n')
    assert text.count(old) == 1, (name, 'mutation did not have exactly one target')
    newline = '\r\n' if b'\r\n' in original else '\n'
    try:
        path.write_bytes(text.replace(old, new).replace('\n', newline).encode('utf-8'))
        command = ['cargo', 'test', '-p', 'craft-ai-comfyui', '--test', target, test, '-j', '8']
        proc = subprocess.Popen(command, cwd=ROOT, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
        timed_out = False
        try:
            log, _ = proc.communicate(timeout=TIMEOUTS.get(name, 180))
        except subprocess.TimeoutExpired:
            # A test that hangs has failed. Kill the whole tree so no test binary lingers.
            timed_out = True
            subprocess.run(['taskkill', '/T', '/F', '/PID', str(proc.pid)], capture_output=True)
            log, _ = proc.communicate()
            log += '\nTIMED OUT: the mutated code made the test hang'
        (OUT / ('mutation-' + name + '.txt')).write_text(scrub(log), encoding='utf-8')
        caught = timed_out or (proc.returncode != 0 and 'test result: FAILED' in log)
        results.append({'mutation': name, 'caught_by_runtime_test': caught, 'timed_out': timed_out})
        print(name, 'CAUGHT' if caught else 'NOT CAUGHT', flush=True)
    finally:
        path.write_bytes(original)
        os.utime(path, None)
    (OUT / (GROUP + '-mutations.json')).write_text(json.dumps(results, indent=2), encoding='utf-8')
assert all(result['caught_by_runtime_test'] for result in results), 'A safety mutation survived'
print(json.dumps({'mutations': len(results), 'caught': sum(result['caught_by_runtime_test'] for result in results)}))
