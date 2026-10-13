#!/usr/bin/env node
/**
 * Record the real, anonymous NERUDO app with an explicitly labelled narration layer.
 * Uses installed Playwright/Chromium and ffmpeg/ffprobe; approved neural or external narration is required.
 * No dependencies are installed and every browser request is read-only.
 *
 * node cf/scripts/record_submission.mjs --audio-template
 * node cf/scripts/record_submission.mjs --prepare --audio-manifest /path/narration.json
 * node cf/scripts/record_submission.mjs --renarrate --audio-manifest /path/narration.json --raw-video /path/browser.webm --caption-python /path/python-with-pillow
 * node cf/scripts/record_submission.mjs --preflight --ready
 * node cf/scripts/record_submission.mjs --record --ready --browser-channel chrome  # capture only
 * node cf/scripts/record_submission.mjs --resume-capture /path/raw-dir --calibration /path/calibration.json --capture-script /path/preserved-capture.mjs --caption-python /path/python-with-pillow
 *
 * `--ready` is the caller's explicit assertion that production and demo are ready.
 * Override narration/paths/actions in the generated recording-plan.json before recording.
 */
import { createRequire } from 'node:module';
import { spawnSync, spawn } from 'node:child_process';
import { promises as fs } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import crypto from 'node:crypto';

const require = createRequire(import.meta.url);
const args = process.argv.slice(2);
const flag = (key) => args.includes(`--${key}`);
const arg = (key, fallback) => {
  const i = args.indexOf(`--${key}`);
  return i < 0 ? fallback : args[i + 1];
};
const output = path.resolve(arg('output', path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../../../competition-submission')));
const base = new URL(arg('base', 'https://nerudo-web.patrickdeutsch.workers.dev/'));
if (base.protocol !== 'https:' || base.search || base.username || base.password || base.pathname !== '/') {
  throw new Error('Use a public HTTPS origin without personal-link keys or credentials.');
}
let allowedPublicOrigins = new Set([base.origin]);
function publicOrigins(plan) {
  const origins = plan.allowedPublicOrigins || [base.origin];
  if (!Array.isArray(origins) || !origins.includes(base.origin)) throw new Error('allowedPublicOrigins must explicitly include the primary public origin.');
  return new Set(origins.map((value) => {
    const url = new URL(value);
    if (url.protocol !== 'https:' || url.username || url.password || url.search || url.hash || url.pathname !== '/' || value !== url.origin) throw new Error('Each allowed public origin must be a credential-free HTTPS origin.');
    return url.origin;
  }));
}
const demo = arg('demo', 'nerudo/demo');
const source = arg('source', 'nerudo/nerudo');
const configPath = path.resolve(arg('plan', path.join(output, 'recording-plan.json')));
const videoPath = path.resolve(arg('video-output', path.join(output, flag('renarrate') ? 'nerudo-submission-natural.mp4' : 'nerudo-submission-visuals.mp4')));
const dimensions = { width: 1600, height: 1000 };
const audioManifestPath = arg('audio-manifest', null);
const transitionSeconds = Number(arg('transition-seconds', '0'));
const maxTempoDelta = 0; // Natural neural speech is never sped up or stretched.
const maxSilenceSeconds = Number(arg('max-silence-seconds', '1.5'));
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const run = (bin, argv) => {
  const result = spawnSync(bin, argv, { encoding: 'utf8', maxBuffer: 8 * 1024 * 1024 });
  if (result.status !== 0) throw new Error(`${bin} failed: ${result.stderr || result.error || result.stdout}`);
  return result.stdout;
};
const duration = (file) => Number(run('ffprobe', ['-v', 'error', '-show_entries', 'format=duration', '-of', 'default=noprint_wrappers=1:nokey=1', file]).trim());
const time = (seconds, separator = ':') => {
  const ms = Math.round(seconds * 1000);
  return `${String(Math.floor(ms / 3600000)).padStart(2, '0')}:${String(Math.floor(ms / 60000) % 60).padStart(2, '0')}:${String(Math.floor(ms / 1000) % 60).padStart(2, '0')}${separator === ',' ? ',' : '.'}${String(ms % 1000).padStart(3, '0')}`;
};
const defaults = () => ({
  version: 1,
  publicOrigin: base.origin,
  disclosure: 'Actual public-site interactions. Product demo is scripted. Historical model and scale evidence is dated and scoped. Narration requires an approved high-quality natural female voice.',
  scenes: [
    {
      id: 'purpose', title: '01 / A Git platform for concurrent coding agents', label: 'Public product · scripted six-client demo', seconds: 55,
      path: '/', expect: 'Repositories',
      actions: [{ at: 12, type: 'goto', path: `/${demo}/graph`, expect: 'Graph' }, { at: 31, type: 'scroll', amount: 330 }],
      narration: 'NERUDO is a Git platform built around concurrent coding agents. This is the public application, and this repository is a scripted six-client product demonstration. The clients exercise real Git forks, submissions, checks, and receipts; they are not six live language models. Each agent works in an isolated Artifacts fork. A Durable Object provides one authoritative integration path for this repository. Instead of every agent racing to update main, the sequencer groups compatible changes, checks the integrated tree, and publishes a batch. The graph records what landed and how. Tasks, attempts, preserved context, and actionable receipts form the collaboration contract. The familiar Git repository remains available to ordinary clones and tools.'
    },
    {
      id: 'integration', title: '02 / The integrated check catches a stale caller', label: 'Recorded demo outcome · actual CI receipt', seconds: 60,
      path: arg('ci-pr', null) ? `/${demo}/pulls/${arg('ci-pr')}` : null, expect: 'Checks',
      actions: [{ at: 12, type: 'tab', name: 'Checks' }, { at: 28, type: 'revision', value: '1' }, { at: 42, type: 'tab', name: 'Context' }],
      narration: 'The hard case is a shared API changing while another agent is still using its old shape. A change can pass on its own base and fail after integration. Here the receipt preserves the rejected revision and the diagnostic from the integrated TypeScript check. The agent can fetch the new main, read the relevant context, repair its caller, and resubmit the same logical change as a new revision. This is real checking of the integrated batch tip. It is important to state the guarantee precisely: a passing batch tip does not prove that every intermediate commit in that batch is independently buildable. The revision history lets a reviewer inspect the rejection and subsequent repair, rather than losing the reason for the retry.'
    },
    {
      id: 'attempts', title: '03 / Compare competing attempts and their plans', label: 'Scripted demo · trials do not publish a winner', seconds: 60,
      path: `/${demo}/tasks/${arg('task', 'T-5')}`, expect: 'Compare',
      actions: [{ at: 8, type: 'tab', name: 'Compare' }, { at: 27, type: 'scroll', amount: 360 }, { at: 43, type: 'scroll', amount: 350 }],
      narration: 'An agent-first platform also needs a way to compare alternative solutions. This task has competing attempts. Each attempt is applied to current main and trial checked without publishing it. The view puts verdicts, changed paths, plans, and diffs side by side. A passing trial is evidence for a choice, not permission to merge every competing implementation. The person can pick an attempt; that attempt still gets its own landing check, and the others close as not picked. The plan is preserved with the exact submitted revision, so the reviewer can understand the intent behind the code. These plans came from the scripted demonstration. The next evidence chapter separately identifies work performed by actual coding-model clients.'
    },
    {
      id: 'authority', title: '04 / Human authority stays bound to exact commits', label: 'Recorded approval and release · anonymous read-only view', seconds: 60,
      path: arg('approval-pr', null) ? `/${demo}/pulls/${arg('approval-pr')}` : null, expect: 'Approved',
      actions: [{ at: 24, type: 'goto', path: arg('release', null) ? `/${demo}/releases/release:${arg('release')}` : null, expect: 'signed by' }, { at: 38, type: 'tab', name: 'Signature' }],
      narration: 'Autonomy needs clear governance boundaries. Repository rules determine which changes need a human signature, including sensitive paths and changes to the rules themselves. The approval shown here was already recorded; this anonymous walkthrough performs no signing action. The signature is tied to the exact change and governing rules. Recovery must activate newly landed rules before admitting the next change, including after a crash. A release is another explicit boundary. The sequencer creates the annotated tag only after the release statement is signed. The signature view exposes who signed and what commit was released. Verification can run from outside the service against the released repository, so the authority record is not confined to a screenshot of the interface.'
    },
    {
      id: 'models', title: '05 / Real coding models, including unfinished outcomes', label: 'Fresh isolated run · pinned build 79c259ad · 2/3 complete · acceptance incomplete', seconds: 60,
      path: `/${source}/repo/file%3Acf/evidence/submission-provider-attempt-20261013.json`, expect: 'immutable_attempt',
      actions: [{ at: 12, type: 'text', text: 'hermes' }, { at: 23, type: 'goto', path: `/${source}/repo/file%3Acf/evidence/submission-coordination-20261013.json`, expect: 'TS2554' }, { at: 32, type: 'goto', path: `/${source}/repo/file%3Acf/evidence/submission-provider-attempt-20261013.json`, expect: 'full_fixture_acceptance' }, { at: 35, type: 'text', text: 'full_fixture_acceptance' }, { at: 44, type: 'goto', path: `/${source}/repo/file%3Acf/LLM_WORKFLOWS.md`, expect: 'workflow' }, { at: 49, type: 'text', text: '20261012-054654' }],
      narration: 'This evidence records real native coding-model workflows. The fresh October thirteenth attempt ran Claude, Codex, and Hermes concurrently on an earlier isolated Worker build. Claude and Codex landed; Hermes reached the nine-hundred-second timeout. Two of three workflows completed. Codex’s first revision failed after Claude changed the currency API. It read Claude’s pinned context and landed a repaired second revision. External verification and eleven runtime tests passed, but full acceptance failed because the receipt feature was missing. Claude’s native planner and reviewer children completed. The first attempt is immutable; any Hermes recovery is separate. Dated October twelfth records also show successful three-client workflows, with three verified landings in three batches. These small experiments demonstrate interoperability, not large-scale model concurrency.'
    },
    {
      id: 'scale', title: '06 / Scale measurements with the conditions visible', label: 'Recorded synthetic benchmarks · scope and failures included', seconds: 70,
      path: `/${source}/repo/file%3ABENCHMARKS.md`, expect: '1,000',
      actions: [{ at: 8, type: 'heading', text: 'Many repos at once' }, { at: 30, type: 'scroll', amount: 230 }, { at: 52, type: 'text', text: 'The 2,000-repo run stopped after 131' }],
      narration: 'The scale evidence measures integration, separately from model generation. On one thousand repositories, a five-minute run recorded about nine hundred ninety-four landings per second in the measured window. Two hundred ninety-nine thousand nine hundred eighty-eight of three hundred thousand changes landed, with twelve push failures. The changes were synthetic and disjoint, with no CI. A separate breadth run exercised ten thousand quiet repositories at about five hundred aggregate landings per second. Each synthetic agent submitted one change every twenty seconds. The peak result was one thousand nine hundred seventy-eight per second across two thousand repositories during a healthy ninety-second window. That run stopped during Artifacts failures at one hundred thirty-one seconds. The peak is not a five-minute capacity claim. These conditions and failures are part of the result.'
    },
    {
      id: 'architecture', title: '07 / Cloudflare architecture and verification bounds', label: 'Workers · Durable Objects · Artifacts · isolated CI Worker', seconds: 55,
      path: `/${source}/repo/file%3ABENCHMARKS.md`, expect: 'WASM',
      actions: [{ at: 9, type: 'heading', text: 'Big repos' }, { at: 28, type: 'goto', path: `/${source}/repo/file%3Acf/README.md`, expect: 'Worker' }, { at: 36, type: 'heading', text: 'Components' }],
      narration: 'The implementation uses Workers for the app and Rust W A S M sequencer, Durable Objects for repository coordination, Artifacts for Git storage and isolated forks, and a service-bound CI Worker. Scale comes from independent repositories and batching within each repository; one repository still has a serialized integration authority. A large-repository experiment landed a seven hundred twenty-eight megabyte tip and passed read-back and Git object verification. Its reported fourteen megabyte peak measures W A S M linear memory, not total isolate RAM or arbitrary monorepo CI. Local fault regressions exercise crash recovery, approval rules, and allowance accounting. The source and run instructions let a reviewer reproduce those checks in an isolated environment and inspect the contracts behind the product.'
    },
    {
      id: 'reproduce', title: '08 / Reproduce the work and see what remains', label: 'Public source · MIT OR Apache-2.0 · reproducible evidence', seconds: 60,
      path: `/${source}/repo/file%3ASUBMISSION.md`, expect: 'Reproduce',
      actions: [{ at: 8, type: 'heading', text: 'Reproduce' }, { at: 26, type: 'text', text: 'Artifacts billing' }, { at: 46, type: 'goto', path: `/${source}`, expect: 'NERUDO' }],
      narration: 'The repository includes the CLI, fixtures, deployment instructions, fault harness, benchmark drivers, and recorded evidence. Clone the public source, run the workspace tests and W A S M check, then use an isolated namespace for live reproduction. The cost discussion separates historical campaign spending from paid Artifacts operations and storage, CI, and model inference. This submission has not demonstrated one hundred thousand simultaneous coding agents on one repository, sustained conflicting workloads with general runtime CI, or a production service-level agreement. Its contribution is a working agent collaboration contract: isolated work, integrated checks, useful receipts, preserved intent, competing attempts, and exact human authority. The public application and source make that design concrete and reviewable.'
    }
  ]
});

const sha256 = (value) => crypto.createHash('sha256').update(value).digest('hex');
const fileHash = async (file) => sha256(await fs.readFile(file));
const readPlan = async () => flag('reset-plan') ? defaults() : fs.readFile(configPath, 'utf8').then(JSON.parse).catch(() => defaults());
const normalizedText = (value) => String(value).replace(/\s+/g, ' ').trim();

function requireApprovedProvider(manifest) {
  const p = manifest?.provider;
  if (manifest?.approved !== true || !p || !['neural', 'external_audio'].includes(p.kind) || p.gender !== 'female' || p.quality !== 'high' || !/natural/i.test(p.style || '') || !p.name || !p.voice || (p.kind === 'neural' && !p.model)) {
    throw new Error('NARRATION_APPROVAL_REQUIRED: use an approved high-quality natural female neural provider or approved external audio; provider name/model/voice/kind/gender/style/quality are required. No basic or system-voice fallback exists.');
  }
  if (/\b(samantha|macos|espeak|festival|pico|system voice)\b/i.test(`${p.name} ${p.model || ''} ${p.voice}`)) {
    throw new Error('NARRATION_SOURCE_REJECTED: basic/system speech is prohibited. Supply the selected natural neural voice or approved external narration.');
  }
  return p;
}

async function validatedAudio(plan) {
  if (!audioManifestPath) throw new Error('NARRATION_AUDIO_REQUIRED: pass --audio-manifest for the selected natural female voice. Use --audio-template to print the contract. There is no automatic voice or fallback.');
  const manifestFile = path.resolve(audioManifestPath);
  const manifest = JSON.parse(await fs.readFile(manifestFile, 'utf8'));
  const provider = requireApprovedProvider(manifest);
  if (!Array.isArray(manifest.scenes)) throw new Error('Audio manifest requires a scenes array.');
  if (!Number.isFinite(transitionSeconds) || transitionSeconds < 0 || transitionSeconds > 0.25 || !Number.isFinite(maxTempoDelta) || maxTempoDelta < 0 || maxTempoDelta > 0.05 || !Number.isFinite(maxSilenceSeconds) || maxSilenceSeconds <= 0 || maxSilenceSeconds > 1.5) {
    throw new Error('Natural pacing limits: transition 0–0.25s, unaltered speech tempo, and maximum silence at most1.5s.');
  }
  if (Math.abs(plan.scenes.reduce((total, s) => total + (s.source_scene_seconds ?? s.seconds), 0) - 480) > 0.001) throw new Error('The source_scene_seconds of the retained capture must total exactly480seconds.');
  const inputs = [];
  for (const scene of plan.scenes) {
    const matches = manifest.scenes.filter((s) => s.id === scene.id);
    if (matches.length !== 1) throw new Error(`Audio manifest needs exactly one clip for ${scene.id}.`);
    const clip = matches[0];
    if (clip.transcript_sha256 !== sha256(scene.narration)) throw new Error(`${scene.id}: transcript hash does not match the current narration; regenerate this clip from the current text.`);
    if (!clip.file) throw new Error(`${scene.id}: missing audio file.`);
    const file = path.resolve(path.dirname(manifestFile), clip.file);
    const probe = JSON.parse(run('ffprobe', ['-v', 'error', '-show_format', '-show_streams', '-of', 'json', file]));
    const audio = probe.streams.find((stream) => stream.codec_type === 'audio');
    const seconds = Number(probe.format.duration);
    if (!audio || !Number.isFinite(seconds) || seconds <= 0) throw new Error(`${scene.id}: a valid nonempty audio clip is required.`);
    if (clip.captions) {
      if (!Array.isArray(clip.captions) || !clip.captions.length) throw new Error(`${scene.id}: captions must be a nonempty array when supplied.`);
      let end = 0;
      for (const c of clip.captions) {
        if (!Number.isFinite(c.start) || !Number.isFinite(c.end) || c.start < end || c.end <= c.start || c.end > seconds + 0.1 || !c.text) throw new Error(`${scene.id}: invalid or overlapping sentence timing.`);
        end = c.end;
      }
      if (normalizedText(clip.captions.map((c) => c.text).join(' ')) !== normalizedText(scene.narration)) throw new Error(`${scene.id}: caption text must cover the approved narration exactly.`);
    }
    inputs.push({ scene, clip, file, inputSeconds: seconds, inputSha256: await fileHash(file), sampleRate: audio.sample_rate, channels: audio.channels });
  }
  return { provider, manifestSha256: await fileHash(manifestFile), captionAlignment: manifest.caption_alignment || null, inputs };
}

function detectedSilences(file, minimum = 0.04, threshold = -55) {
  const result = spawnSync('ffmpeg', ['-hide_banner', '-i', file, '-af', `silencedetect=noise=${threshold}dB:d=${minimum}`, '-f', 'null', '-'], { encoding: 'utf8', maxBuffer: 8 * 1024 * 1024 });
  if (result.status !== 0) throw new Error(`Audio silence analysis failed: ${result.stderr}`);
  const ranges = [];
  let start = null;
  for (const line of result.stderr.split('\n')) {
    const begins = /silence_start: ([\d.]+)/.exec(line);
    const ends = /silence_end: ([\d.]+)/.exec(line);
    if (begins) start = Number(begins[1]);
    if (ends && start != null) { ranges.push({ start, end: Number(ends[1]) }); start = null; }
  }
  if (start != null) ranges.push({ start, end: duration(file) });
  return ranges;
}

function loudnessStats(file) {
  const result = spawnSync('ffmpeg', ['-hide_banner', '-i', file, '-af', 'loudnorm=I=-16:TP=-2:LRA=11:print_format=json', '-f', 'null', '-'], { encoding: 'utf8', maxBuffer: 8 * 1024 * 1024 });
  if (result.status !== 0) throw new Error(`Loudness measurement failed: ${result.stderr}`);
  const json = /\{\s*"input_i"[\s\S]*?\}/.exec(result.stderr);
  if (!json) throw new Error('Missing loudness measurement.');
  const measured = JSON.parse(json[0]);
  if (!Number.isFinite(Number(measured.input_i)) || !Number.isFinite(Number(measured.input_tp))) throw new Error('Narration must contain measurable speech.');
  return measured;
}

async function normalizedNarration(file, staging) {
  const before = loudnessStats(file);
  const normalized = path.join(staging, 'narration-normalized.wav');
  const filter = `loudnorm=I=-16:TP=-2:LRA=11:measured_I=${before.input_i}:measured_TP=${before.input_tp}:measured_LRA=${before.input_lra}:measured_thresh=${before.input_thresh}:offset=${before.target_offset}:linear=true:print_format=json`;
  run('ffmpeg', ['-hide_banner', '-loglevel', 'error', '-y', '-i', file, '-af', filter, '-ar', '48000', '-ac', '1', '-c:a', 'pcm_s16le', normalized]);
  const after = loudnessStats(normalized);
  if (Math.abs(Number(after.input_i) + 16) > 1 || Number(after.input_tp) > -1.5) throw new Error('Normalized narration must be near-16LUFS with true peak at most-1.5dBTP.');
  return { file: normalized, provenance: { scope: 'whole_track', targetIntegratedLufs: -16, targetTruePeakDbtp: -2, maxTruePeakDbtp: -1.5, targetLra: 11, tempoAltered: false, before, after, settings: filter } };
}

async function approvedPreparedAudio(plan) {
  requireApprovedProvider({ approved: plan.narration_provenance?.approved, provider: plan.narration_provenance?.provider });
  const expected = plan.narration_provenance?.audioSha256;
  const actual = await fileHash(path.join(output, 'narration.wav'));
  if (!expected || actual !== expected) throw new Error('STALE_NARRATION_AUDIO: narration.wav differs from the approved plan hash. Re-prepare approved audio; no stale voice can be captured or encoded.');
}

function splitCaption(text, start, end) {
  const words = text.trim().split(/\s+/);
  const parts = [];
  let current = [];
  for (const word of words) {
    if (current.length && [...current, word].join(' ').length > 210) { parts.push(current); current = []; }
    current.push(word);
  }
  if (current.length) parts.push(current);
  let at = start;
  return parts.map((part) => {
    const next = at + (end - start) * part.length / words.length;
    const caption = { start: at, end: next, text: part.join(' ') };
    at = next;
    return caption;
  });
}

async function prepare() {
  const plan = await readPlan();
  // Validate approval and every input before touching an existing plan or media.
  const approved = await validatedAudio(plan);
  const staging = path.join(output, `.narration-${new Date().toISOString().replace(/[:.]/g, '-')}`);
  await fs.mkdir(staging, { recursive: true });
  let offset = 0;
  const captions = [];
  const clips = [];
  const audioFiles = [];
  for (const input of approved.inputs) {
    const { scene, clip, file, inputSeconds } = input;
    const trimStart = 0;
    const trimEnd = inputSeconds;
    const trimmedSeconds = inputSeconds;
    if (trimmedSeconds <= 0) throw new Error(`${scene.id}: a nonempty spoken audio clip is required.`);
    scene.source_scene_seconds ??= scene.seconds;
    scene.seconds = trimmedSeconds + transitionSeconds;
    const target = trimmedSeconds;
    const tempo = 1;
    const prepared = path.join(staging, `${scene.id}.wav`);
    run('ffmpeg', ['-hide_banner', '-loglevel', 'error', '-y', '-i', file, '-af', `asetpts=PTS-STARTPTS${transitionSeconds ? `,apad=pad_dur=${transitionSeconds}` : ''},atrim=duration=${scene.seconds}`, '-ar', '48000', '-ac', '1', '-c:a', 'pcm_s16le', prepared]);
    const longSilences = detectedSilences(prepared, maxSilenceSeconds, -45);
    if (longSilences.length) throw new Error(`${scene.id}: narration contains a silence of at least${maxSilenceSeconds}s; regenerate this clip with continuous natural pacing.`);
    audioFiles.push(prepared);
    const timingMethod = clip.captions ? approved.captionAlignment?.method || 'provided_caption_timing' : 'estimated_word_weighted';
    let relative = clip.captions;
    if (relative) relative = relative.map((c) => ({ start: Math.max(0, (c.start - trimStart) / tempo), end: Math.min(target, (c.end - trimStart) / tempo), text: c.text }));
    else {
      const sentences = scene.narration.match(/[^.!?]+[.!?]+(?:\s|$)/g) || [scene.narration];
      const counts = sentences.map((sentence) => sentence.trim().split(/\s+/).length);
      const total = counts.reduce((a, b) => a + b, 0);
      let at = 0;
      relative = sentences.map((sentence, i) => { const end = at + target * counts[i] / total; const c = { start: at, end, text: sentence.trim() }; at = end; return c; });
    }
    for (const c of relative) captions.push(...splitCaption(c.text, offset + c.start, offset + c.end));
    scene.audio = { provider: approved.provider, inputSha256: input.inputSha256, transcriptSha256: clip.transcript_sha256, inputSeconds, trimStartSeconds: trimStart, trimEndSeconds: trimEnd, tempo, spokenSeconds: target, transitionSeconds, leadSeconds: 0, captionTiming: timingMethod };
    clips.push({ id: scene.id, ...scene.audio, preparedSha256: await fileHash(prepared), sampleRate: input.sampleRate, channels: input.channels });
    offset += scene.seconds;
  }
  const list = path.join(staging, 'concat.txt');
  await fs.writeFile(list, audioFiles.map((file) => `file '${file.replaceAll("'", "'\\''")}'`).join('\n') + '\n');
  let narration = path.join(staging, 'narration.wav');
  run('ffmpeg', ['-hide_banner', '-loglevel', 'error', '-y', '-f', 'concat', '-safe', '0', '-i', list, '-c:a', 'pcm_s16le', narration]);
  if (offset < 300 || offset > 600 || Math.abs(duration(narration) - offset) > 0.05) throw new Error('Natural narration must stay within5–10minutes and match the summed chapter durations.');
  const normalization = await normalizedNarration(narration, staging);
  narration = normalization.file;
  const wholeTrackSilences = detectedSilences(narration, maxSilenceSeconds, -45);
  if (wholeTrackSilences.length) throw new Error(`Narration has a gap of at least${maxSilenceSeconds}s at a chapter join; regenerate the affected clips.`);
  plan.disclosure = `Actual public-site interactions. Product demo is scripted. Historical model and scale evidence is dated and scoped. Narration: ${approved.provider.name}, ${approved.provider.model || approved.provider.kind}, voice ${approved.provider.voice}; approved natural female narration.`;
  plan.narration_provenance = { approved: true, provider: approved.provider, manifestSha256: approved.manifestSha256, captionAlignment: approved.captionAlignment, preparedAt: new Date().toISOString(), audioSha256: await fileHash(narration), pacing: { followsAudio: true, transitionSeconds, maxTempoDelta, maxSilenceSeconds, wholeTrackSilences }, normalization: normalization.provenance, clips };
  const transcript = ['# NERUDO narrated submission', '', plan.disclosure, ''];
  const storyboard = ['# Recording storyboard', '', '| Time | Chapter | Public page / actions | Disclosure |', '|---|---|---|---|'];
  let at = 0;
  for (const scene of plan.scenes) {
    transcript.push(`## ${time(at).slice(0, 8)} — ${scene.title}`, '', `*${scene.label}*`, '', scene.narration, '');
    storyboard.push(`| ${time(at).slice(3, 8)}–${time(at + scene.seconds).slice(3, 8)} | ${scene.title} | ${scene.path}; ${scene.actions.map((a) => `${(a.at * scene.seconds / scene.source_scene_seconds).toFixed(2)}s ${a.type} ${a.path || a.name || a.text || a.amount || a.value || ''}`).join('; ')} | ${scene.label} |`);
    at += scene.seconds;
  }
  await fs.copyFile(narration, path.join(output, 'narration.wav'));
  await fs.writeFile(path.join(output, 'transcript.md'), transcript.join('\n'));
  await fs.writeFile(path.join(output, 'storyboard.md'), storyboard.join('\n') + '\n');
  await fs.writeFile(path.join(output, 'captions.json'), JSON.stringify(captions, null, 2) + '\n');
  await fs.writeFile(path.join(output, 'captions.srt'), captions.map((c, i) => `${i + 1}\n${time(c.start, ',')} --> ${time(c.end, ',')}\n${c.text}\n`).join('\n'));
  await fs.writeFile(configPath, JSON.stringify(plan, null, 2) + '\n');
  console.log(JSON.stringify({ prepared: true, chapters: plan.scenes.length, seconds: offset, provider: approved.provider, pacing: plan.narration_provenance.pacing }));
  return { plan, captions, staging };
}

async function pageAction(page, action) {
  if (action.type === 'goto') {
    if (!action.path) throw new Error('A final demo PR/release URL is required. Fill recording-plan.json.');
    const url = new URL(action.path, base);
    if (!allowedPublicOrigins.has(url.origin) || url.search || url.hash || url.username || url.password) throw new Error('Recording only accepts anonymous pages on explicitly allowed public origins without query keys.');
    const response = await page.goto(url.href, { waitUntil: 'domcontentloaded', timeout: 60000 });
    if (!response || response.status() >= 400) throw new Error(`Public page failed: ${response?.status()} ${url.pathname}`);
    await page.locator('body').waitFor();
    if (action.expect) await page.getByText(action.expect, { exact: false }).first().waitFor({ timeout: 45000 });
    const unavailable = await page.locator('body').innerText();
    if (/Application error:|File unavailable|We couldn’t read this file|Repository list unavailable/.test(unavailable)) throw new Error(`Visible error state at ${url.pathname}`);
  } else if (action.type === 'tab') {
    const pattern = new RegExp(`^${action.name.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}(?:\\s|\\(|$)`, 'i');
    const tab = page.getByRole('tab', { name: pattern });
    const button = page.getByRole('button', { name: pattern });
    const locator = tab.or(button).first();
    await locator.waitFor({ state: 'visible', timeout: 15000 });
    await locator.click();
    if (await locator.getAttribute('role') === 'tab') await page.waitForFunction((element) => element?.getAttribute('aria-selected') === 'true', await locator.elementHandle(), { timeout: 1500 });
    return { tab: { name: action.name, selected: await locator.getAttribute('aria-selected') } };
  } else if (action.type === 'revision') {
    await page.getByRole('combobox', { name: 'Revision' }).selectOption(action.value);
    return { revision: action.value, selectedTabs: await page.getByRole('tab', { selected: true }).allTextContents() };
  } else if (action.type === 'scroll') {
    await page.mouse.move(1200, 620);
    await page.mouse.wheel(0, action.amount);
  } else if (action.type === 'text' || action.type === 'heading') {
    const target = await productTarget(page, action.type === 'heading' ? { role: 'heading', name: action.text, exact: true } : { text: action.text, exact: false });
    await target.evaluate((element) => element.scrollIntoView({ block: 'center', inline: 'nearest', behavior: 'instant' }));
    return { target: await targetEvidence(target) };
  } else if (action.type === 'focus' || action.type === 'highlight') {
    const target = await productTarget(page, action.target);
    await target.evaluate((element) => element.scrollIntoView({ block: 'center', inline: 'nearest', behavior: 'instant' }));
    const evidence = await targetEvidence(target);
    return { target: evidence };
  } else if (action.type === 'explain') {
    await explanation(page, action);
    return { explanation: { label: action.label, title: action.title, cards: action.cards, result: action.result } };
  } else if (['clear_attention', 'clear_explanation', 'clear_visuals'].includes(action.type)) {
    await page.evaluate((kind) => {
      const ids = kind === 'clear_attention' ? ['submission-attention'] : kind === 'clear_explanation' ? ['submission-explanation'] : ['submission-attention', 'submission-explanation'];
      ids.forEach((id) => document.getElementById(id)?.remove());
    }, action.type);
  } else throw new Error(`Unknown recording action ${action.type}`);
}

async function semanticAssertions(page, action) {
  const list = (value) => value === undefined ? [] : Array.isArray(value) ? value : [value];
  const expectedProduct = list(action.expected_product_text);
  const expectedVisible = list(action.expected_visible_text);
  const expectedHeader = list(action.expected_header_state);
  if (![...expectedProduct, ...expectedVisible, ...expectedHeader].length) return null;
  if ([...expectedProduct, ...expectedVisible, ...expectedHeader].some((value) => typeof value !== 'string' || !value.trim())) throw new Error('Semantic assertions must be nonempty strings or arrays of strings.');
  const deadline = performance.now() + 5000;
  let observed = null;
  do {
    const product = action.target ? await productTarget(page, action.target).then((target) => target.evaluate((element) => element.innerText || element.textContent || '')) : await page.evaluate((injected) => {
      const copy = document.body.cloneNode(true);
      // Pair nodes before pruning to keep sibling indexes stable.
      const walk = (original, clone) => {
        if (original.nodeType === 1 && (original.matches(`${injected},script,style,noscript`) || getComputedStyle(original).display === 'none' || getComputedStyle(original).visibility === 'hidden')) { clone.remove(); return; }
        const children = [...clone.childNodes];
        [...original.childNodes].forEach((child, i) => walk(child, children[i]));
      };
      walk(document.body, copy);
      return copy.textContent || '';
    }, injectedSelector);
    const header = await page.locator('article.detail .d-head .chip').allTextContents();
    const visible = [];
    for (const text of expectedVisible) {
      let matched = null;
      for (const item of await page.getByText(text, { exact: false }).all()) {
        const candidate = await item.evaluate((element, injected) => {
          const r = element.getBoundingClientRect();
          const bottom = document.getElementById('submission-recording-overlay')?.getBoundingClientRect().top ?? innerHeight;
          if (element.closest(injected) || ['BODY', 'HTML'].includes(element.tagName) || r.width <= 0 || r.height <= 0 || r.left < 0 || r.top < 0 || r.right > innerWidth || r.bottom > bottom - 6) return null;
          for (let parent = element.parentElement; parent; parent = parent.parentElement) {
            const style = getComputedStyle(parent); const clip = parent.getBoundingClientRect();
            if (/auto|scroll|hidden|clip/.test(`${style.overflowX} ${style.overflowY}`) && (r.left < clip.left || r.right > clip.right || r.top < clip.top || r.bottom > clip.bottom)) return null;
          }
          return (element.innerText || element.textContent || '').trim();
        }, injectedSelector);
        if (candidate !== null) { matched = candidate; break; }
      }
      visible.push({ expected: text, observed: matched });
    }
    observed = { product: expectedProduct.map((text) => ({ expected: text, present: normalizedText(product).includes(normalizedText(text)) })), visible, header, expectedHeader };
    if (observed.product.every((item) => item.present) && visible.every((item) => item.observed !== null) && expectedHeader.every((text) => header.some((actual) => normalizedText(actual) === normalizedText(text)))) return observed;
    await sleep(50);
  } while (performance.now() < deadline);
  throw new Error(`Semantic visual assertion failed: ${JSON.stringify(observed)}`);
}

async function usePage(page, action, visualContext) {
  let evidence = await pageAction(page, action);
  if (visualContext) await overlay(page, visualContext.scene, visualContext.caption || '');
  const assertions = await semanticAssertions(page, action);
  if (action.type === 'highlight') {
    const target = await productTarget(page, action.target);
    await target.evaluate((element) => element.scrollIntoView({ block: 'center', inline: 'nearest', behavior: 'instant' }));
    evidence = { target: await targetEvidence(target) };
    await highlight(target, action);
  }
  return { ...evidence, ...(assertions ? { assertions } : {}) };
}

const injectedSelector = '#submission-recording-overlay, #submission-attention, #submission-explanation';

async function productTarget(page, spec) {
  if (!spec || [spec.selector, spec.role, spec.text].filter((value) => value !== undefined).length !== 1) throw new Error('A target needs exactly one observed selector, role/name, or text locator.');
  const query = spec.selector ? page.locator(spec.selector) : spec.role ? page.getByRole(spec.role, { name: spec.name, exact: spec.exact ?? true }) : page.getByText(spec.text, { exact: spec.exact ?? true });
  const index = spec.nth ?? 0;
  if (!Number.isInteger(index) || index < 0) throw new Error('Target nth must be a nonnegative product-only index.');
  await query.first().waitFor({ state: 'attached', timeout: 5000 });
  const candidates = [];
  for (const item of await query.all()) {
    if (await item.isVisible() && await item.evaluate((element, injected) => !['BODY', 'HTML'].includes(element.tagName) && !element.closest(injected), injectedSelector)) candidates.push(item);
  }
  const target = candidates[index];
  if (!target) throw new Error('Target locator did not identify product content; recording annotations are excluded.');
  await target.waitFor({ state: 'visible', timeout: 5000 });
  return target;
}

async function targetEvidence(target) {
  return target.evaluate((element, injected) => {
    const box = element.getBoundingClientRect();
    const captionTop = document.getElementById('submission-recording-overlay')?.getBoundingClientRect().top ?? innerHeight;
    if (element.closest(injected) || box.width <= 0 || box.height <= 0 || box.left < 0 || box.top < 0 || box.right > innerWidth || box.bottom > captionTop - 6) {
      throw new Error('Attention target must be fully visible above the caption band; choose a smaller product element or focus it earlier.');
    }
    for (let parent = element.parentElement; parent; parent = parent.parentElement) {
      const style = getComputedStyle(parent); const clip = parent.getBoundingClientRect();
      if (/auto|scroll|hidden|clip/.test(`${style.overflowX} ${style.overflowY}`) && (box.left < clip.left || box.right > clip.right || box.top < clip.top || box.bottom > clip.bottom)) throw new Error('Attention target is clipped by a scrolling pane; center a smaller visible element.');
    }
    return { text: (element.innerText || element.textContent || '').trim().slice(0, 1200), rect: { x: box.x, y: box.y, width: box.width, height: box.height }, captionTop };
  }, injectedSelector);
}

async function highlight(target, action) {
  await target.evaluate((element, options) => {
    document.getElementById('submission-attention')?.remove();
    const layer = document.createElement('div');
    layer.id = 'submission-attention';
    layer.style.cssText = 'position:fixed;inset:0;pointer-events:none;z-index:2147483645;font-family:system-ui,sans-serif;';
    const frame = document.createElement('div');
    frame.style.cssText = 'position:absolute;box-sizing:border-box;border:4px solid #e8a63e;border-radius:5px;box-shadow:0 0 0 2px #fff8df,0 0 22px rgba(232,166,62,.65);background:rgba(232,166,62,.06);';
    const label = document.createElement('div');
    label.textContent = options.label || 'Look here';
    label.style.cssText = 'position:absolute;padding:10px 16px;background:#234e3e;color:#fff8df;border:2px solid #fff8df;border-radius:5px;font-size:23px;font-weight:700;line-height:1.22;max-width:390px;box-shadow:0 3px 14px #0005;';
    const svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
    svg.style.cssText = 'position:absolute;inset:0;width:100%;height:100%;overflow:visible;';
    const edge = document.createElementNS(svg.namespaceURI, 'path');
    edge.setAttribute('stroke', '#fff8df'); edge.setAttribute('stroke-width', '11'); edge.setAttribute('fill', 'none'); edge.setAttribute('stroke-linecap', 'round');
    const arrow = document.createElementNS(svg.namespaceURI, 'path');
    arrow.setAttribute('stroke', '#234e3e'); arrow.setAttribute('stroke-width', '6'); arrow.setAttribute('fill', 'none'); arrow.setAttribute('stroke-linecap', 'round'); arrow.setAttribute('stroke-linejoin', 'round');
    svg.append(edge, arrow); layer.append(frame, svg, label); document.body.append(layer);
    const now = () => {
      const clock = window.__nerudoRecordingClock;
      return options.audioExpiresAt !== undefined && clock ? clock.at + (clock.paused ? 0 : (performance.now() - clock.started) / 1000) : performance.now() / 1000;
    };
    const expires = options.audioExpiresAt ?? now() + (options.duration ?? 6);
    const update = () => {
      if (!layer.isConnected) return;
      if (!element.isConnected || now() >= expires) { layer.remove(); return; }
      const r = element.getBoundingClientRect();
      const bottom = document.getElementById('submission-recording-overlay')?.getBoundingClientRect().top ?? innerHeight;
      if (r.top < 0 || r.left < 0 || r.right > innerWidth || r.bottom > bottom - 6) { layer.style.visibility = 'hidden'; requestAnimationFrame(update); return; }
      layer.style.visibility = 'visible';
      Object.assign(frame.style, { left: `${r.left - 3}px`, top: `${r.top - 3}px`, width: `${r.width + 6}px`, height: `${r.height + 6}px` });
      const labelWidth = label.offsetWidth; const labelHeight = label.offsetHeight;
      const above = r.top - labelHeight - 58 >= 12;
      const y = above ? r.top - labelHeight - 58 : Math.min(bottom - labelHeight - 18, r.bottom + 58);
      const x = Math.max(12, Math.min(innerWidth - labelWidth - 12, r.left + r.width / 2 - labelWidth / 2));
      Object.assign(label.style, { left: `${x}px`, top: `${y}px` });
      const endX = Math.max(r.left + 10, Math.min(r.right - 10, x + labelWidth / 2));
      const endY = above ? r.top - 5 : r.bottom + 5;
      const fromY = above ? y + labelHeight + 2 : y - 2;
      const direction = above ? 1 : -1;
      const line = `M ${x + labelWidth / 2} ${fromY} L ${endX} ${endY}`;
      const head = ` M ${endX - 15} ${endY - direction * 18} L ${endX} ${endY} L ${endX + 15} ${endY - direction * 18}`;
      edge.setAttribute('d', line + head); arrow.setAttribute('d', line + head);
      requestAnimationFrame(update);
    };
    update();
  }, { label: action.label, duration: action.duration, audioExpiresAt: action.audioExpiresAt });
}

async function explanation(page, action) {
  if (!/^Explanation\b/i.test(action.label || '') || !Array.isArray(action.cards) || action.cards.length !== 2 || !action.result) throw new Error('Explanation annotations require an explicit Explanation label, two cards, and a result.');
  await page.evaluate((options) => {
    document.getElementById('submission-explanation')?.remove();
    const panel = document.createElement('aside'); panel.id = 'submission-explanation';
    panel.style.cssText = 'position:fixed;top:68px;right:26px;width:750px;max-width:calc(100vw - 52px);padding:21px;box-sizing:border-box;z-index:2147483644;pointer-events:none;background:#f7f6f2;border:3px solid #38574a;border-radius:7px;box-shadow:0 8px 32px #0005;color:#282722;font-family:system-ui,sans-serif;';
    const disclosure = document.createElement('div'); disclosure.textContent = options.label; disclosure.style.cssText = 'color:#536b60;font-size:15px;font-weight:700;margin-bottom:8px;';
    const title = document.createElement('div'); title.textContent = options.title; title.style.cssText = 'font-size:29px;font-weight:750;margin-bottom:16px;';
    const cards = document.createElement('div'); cards.style.cssText = 'display:grid;grid-template-columns:1fr 1fr;gap:14px;';
    for (const c of options.cards) {
      const card = document.createElement('div'); card.style.cssText = 'min-width:0;background:#ebe8db;border:1px solid #c9c4b9;padding:15px;';
      const heading = document.createElement('div'); heading.textContent = c.title; heading.style.cssText = 'font-size:19px;font-weight:700;margin-bottom:12px;';
      const code = document.createElement('pre'); code.textContent = c.code; code.style.cssText = 'font:26px/1.4 ui-monospace,monospace;white-space:pre-wrap;overflow-wrap:anywhere;margin:0;';
      card.append(heading, code); cards.append(card);
    }
    const result = document.createElement('div'); result.textContent = options.result; result.style.cssText = 'background:#f5e5d7;border-left:5px solid #ba5739;padding:12px 14px;margin-top:15px;font-size:21px;font-weight:650;line-height:1.35;';
    panel.append(disclosure, title, cards, result); document.body.append(panel);
    const captionTop = document.getElementById('submission-recording-overlay')?.getBoundingClientRect().top ?? innerHeight;
    if (panel.getBoundingClientRect().bottom > captionTop - 12) { panel.remove(); throw new Error('Explanation must remain above the caption band; shorten the cards/result.'); }
    const wallExpires = performance.now() + (options.duration ?? 8) * 1000;
    const expire = () => {
      if (!panel.isConnected) return;
      const clock = window.__nerudoRecordingClock;
      const audioAt = clock ? clock.at + (clock.paused ? 0 : (performance.now() - clock.started) / 1000) : null;
      if (options.audioExpiresAt !== undefined && audioAt !== null ? audioAt >= options.audioExpiresAt : performance.now() >= wallExpires) panel.remove();
      else requestAnimationFrame(expire);
    };
    expire();
  }, action);
}

async function browserAudioClock(page, at, paused) {
  await page.evaluate(({ at, paused }) => { window.__nerudoRecordingClock = { at, paused, started: performance.now() }; }, { at, paused });
}

function readyFrameSegments(segments) {
  return segments.map((segment) => {
    const startFrame = Math.round(segment.audio_start * 25);
    const endFrame = Math.round(segment.audio_end * 25);
    return { ...segment, encoded_audio_start: startFrame / 25, encoded_audio_end: endFrame / 25, encoded_frames: endFrame - startFrame, encoded_seconds: (endFrame - startFrame) / 25 };
  }).filter((segment) => segment.encoded_frames > 0);
}

async function encodeReadySegments(rawVideo, segments, directory) {
  await fs.mkdir(directory, { recursive: true });
  const clips = [];
  for (const [i, segment] of segments.entries()) {
    const file = path.join(directory, `${String(i).padStart(3, '0')}.mp4`);
    run('ffmpeg', ['-hide_banner', '-loglevel', 'error', '-y', '-ss', String(segment.source_start), '-i', rawVideo, '-an', '-vf', `trim=duration=${segment.seconds},setpts=PTS-STARTPTS,fps=25,tpad=stop_mode=clone:stop_duration=0.08,trim=duration=${segment.encoded_seconds}`, '-frames:v', String(segment.encoded_frames), '-c:v', 'libx264', '-preset', 'fast', '-crf', '21', '-bf', '0', '-pix_fmt', 'yuv420p', file]);
    clips.push(file);
    console.log(JSON.stringify({ readySegment: i + 1, total: segments.length, scene: segment.scene, audioStart: segment.encoded_audio_start, seconds: segment.encoded_seconds }));
  }
  const list = path.join(directory, 'concat.txt');
  await fs.writeFile(list, clips.map((file) => `file '${file.replaceAll("'", "'\\''")}'`).join('\n') + '\n');
  return list;
}

function choreography(plan, captions) {
  if (plan.choreography?.clock !== 'audio') throw new Error('A fresh recording requires choreography.clock="audio" and explicit narration timestamps; legacy proportional action timers are rejected.');
  const tolerance = plan.choreography.maxActionLatenessSeconds ?? 0.5;
  if (!Number.isFinite(tolerance) || tolerance < 0.04 || tolerance > 1) throw new Error('maxActionLatenessSeconds must be0.04–1seconds.');
  const allowed = new Set(['goto', 'tab', 'revision', 'scroll', 'text', 'heading', 'focus', 'highlight', 'explain', 'clear_attention', 'clear_explanation', 'clear_visuals']);
  let offset = 0;
  const scenes = plan.scenes.map((scene) => {
    if (!Number.isFinite(scene.seconds) || scene.seconds <= 0) throw new Error('Chapters need positive narration durations.');
    let previous = -1;
    const actions = (scene.actions || []).map((input) => {
      const action = { ...input };
      if (!allowed.has(action.type)) throw new Error(`Unknown choreography action ${action.type}.`);
      if (['focus', 'highlight'].includes(action.type) && (!action.target || [action.target.selector, action.target.role, action.target.text].filter((value) => value !== undefined).length !== 1)) throw new Error('Focus/highlight actions need exactly one product target selector, role/name, or text.');
      if (action.cue) {
        const needle = normalizedText(action.cue.text);
        if (!needle) throw new Error('Caption cue text is required.');
        const matches = captions.filter((c) => c.start >= offset - 0.001 && c.end <= offset + scene.seconds + 0.001 && (action.cue.exact ? normalizedText(c.text) === needle : normalizedText(c.text).includes(needle)));
        if (matches.length !== 1) throw new Error(`${scene.id}: cue must match exactly one canonical caption.`);
        const edge = action.cue.edge || 'start';
        if (!['start', 'end'].includes(edge)) throw new Error('Cue edge must be start or end.');
        const at = matches[0][edge] - offset + (action.cue.offset ?? 0);
        if (action.at !== undefined && Math.abs(action.at - at) > 0.04) throw new Error(`${scene.id}: explicit at differs from its caption cue.`);
        action.at = at;
        action.resolvedCue = matches[0];
      }
      if (!Number.isFinite(action.at) || action.at < 0 || action.at >= scene.seconds || action.at < previous) throw new Error(`${scene.id}: actions must have ordered chapter-relative audio timestamps.`);
      if (action.readyBy !== undefined && (!Number.isFinite(action.readyBy) || action.readyBy < action.at || action.readyBy >= scene.seconds)) throw new Error(`${scene.id}: readyBy must follow at within the chapter.`);
      if (['highlight', 'explain'].includes(action.type)) {
        action.duration ??= Math.min(action.type === 'highlight' ? 6 : 8, scene.seconds - action.at);
        if (!Number.isFinite(action.duration) || action.duration <= 0 || action.at + action.duration > scene.seconds + 0.001) throw new Error(`${scene.id}: annotation duration must fit its chapter.`);
      }
      if (action.type === 'explain' && (!/^Explanation\b/i.test(action.label || '') || !Array.isArray(action.cards) || action.cards.length !== 2 || !action.title || !action.result || action.cards.some((c) => !c.title || !c.code))) throw new Error('Explanation annotations need a disclosure, title, two cards, and result.');
      previous = action.at;
      return action;
    });
    const setup = scene.setup || [];
    if (!Array.isArray(setup) || setup.some((a) => !allowed.has(a.type) || ['highlight', 'explain'].includes(a.type) || a.at !== undefined || a.cue !== undefined)) throw new Error(`${scene.id}: setup must be untimed read-only page/focus actions; annotations belong to the audio timeline.`);
    const result = { ...scene, setup, offset, actions };
    offset += scene.seconds;
    return result;
  });
  return { scenes, seconds: offset, tolerance };
}

async function overlay(page, scene, caption = '') {
  await page.evaluate(({ scene, caption }) => {
    let element = document.getElementById('submission-recording-overlay');
    if (!element) {
      element = document.createElement('div');
      element.id = 'submission-recording-overlay';
      element.style.cssText = 'position:fixed;bottom:0;left:0;right:0;z-index:2147483647;pointer-events:none;background:rgba(247,246,242,.97);border-top:1px solid #c9c4b9;padding:13px 26px 16px;color:#282722;font-family:system-ui,sans-serif;box-sizing:border-box;min-height:115px;';
      document.body.appendChild(element);
    }
    element.replaceChildren();
    const heading = document.createElement('div');
    heading.style.cssText = 'font-size:17px;font-weight:650;letter-spacing:.02em';
    heading.textContent = scene.title;
    const disclosure = document.createElement('div');
    disclosure.style.cssText = 'font-size:13px;color:#67635b;margin-top:3px';
    disclosure.textContent = `${scene.label} · ${location.origin}${location.pathname}`;
    const subtitle = document.createElement('div');
    subtitle.style.cssText = 'font-size:20px;line-height:1.28;margin-top:7px;max-width:1530px;';
    subtitle.textContent = caption;
    element.append(heading, disclosure, subtitle);
  }, { scene, caption });
}

async function record(preflight = false) {
  if (!flag('ready')) throw new Error('Wait for the final production-ready signal, then pass --ready.');
  const plan = JSON.parse(await fs.readFile(configPath, 'utf8'));
  if (arg('ci-pr', null)) plan.scenes.find((s) => s.id === 'integration').path = `/${demo}/pulls/${arg('ci-pr')}`;
  if (arg('approval-pr', null)) plan.scenes.find((s) => s.id === 'authority').path = `/${demo}/pulls/${arg('approval-pr')}`;
  if (arg('release', null)) plan.scenes.find((s) => s.id === 'authority').actions.find((a) => a.type === 'goto').path = `/${demo}/releases/release:${arg('release')}`;
  if (arg('task', null)) plan.scenes.find((s) => s.id === 'attempts').path = `/${demo}/tasks/${arg('task')}`;
  const captions = JSON.parse(await fs.readFile(path.join(output, 'captions.json'), 'utf8'));
  if (plan.publicOrigin !== base.origin) throw new Error('Plan origin differs from --base; regenerate the plan.');
  const schedule = choreography(plan, captions);
  allowedPublicOrigins = publicOrigins(plan);
  await approvedPreparedAudio(plan);
  const audioHash = await fileHash(path.join(output, 'narration.wav'));
  const captionsHash = await fileHash(path.join(output, 'captions.json'));
  if (Math.abs(duration(path.join(output, 'narration.wav')) - schedule.seconds) > 0.02) throw new Error('Chapters must retain the approved narration duration; fresh recording does not rescale audio.');
  const { chromium } = require('playwright');
  const browser = await chromium.launch({ headless: !flag('headed'), ...(arg('browser-channel', plan.browserChannel) ? { channel: arg('browser-channel', plan.browserChannel) } : {}) });
  const rawDir = path.join(output, preflight ? 'preflight' : `raw-${new Date().toISOString().replace(/[:.]/g, '-')}`);
  await fs.mkdir(rawDir, { recursive: true });
  const context = await browser.newContext({ viewport: dimensions, deviceScaleFactor: 1, ...(preflight ? {} : { recordVideo: { dir: rawDir, size: dimensions } }) });
  if (plan.experience) {
    if (!['modern', 'retro'].includes(plan.experience)) throw new Error('experience must be modern or retro.');
    await context.addCookies([...allowedPublicOrigins].map((origin) => ({ name: 'nerudo_experience', value: plan.experience, url: origin, sameSite: 'Lax' })));
  }
  await context.route('**/*', (route) => ['GET', 'HEAD', 'OPTIONS'].includes(route.request().method()) ? route.continue() : route.abort());
  const rawClockStarted = performance.now();
  const page = await context.newPage();
  const journal = [];
  const errors = [];
  page.on('pageerror', (error) => errors.push(String(error)));
  let chapterStarted = null;
  let frozenAudioAt = 0;
  let timer = null;
  let activeScene = null;
  let lastOverlay = null;
  let overlayBusy = false;
  let recordedAt = null;
  let previewAt = 0;
  const segments = [];
  let openSegment = null;
  let rawClockStopped = null;
  const rawWallAt = () => (performance.now() - rawClockStarted) / 1000;
  const elapsed = () => preflight ? previewAt : frozenAudioAt + (chapterStarted === null ? 0 : (performance.now() - chapterStarted) / 1000);
  const refreshOverlay = async () => {
    if (!activeScene || overlayBusy) return;
    const at = elapsed();
    const caption = captions.find((c) => c.start <= at && at < c.end)?.text || '';
    const key = `${activeScene.id}:${page.url()}:${caption}`;
    if (key === lastOverlay) return;
    overlayBusy = true;
    try { await overlay(page, activeScene, caption); lastOverlay = key; }
    catch { /* A navigation can replace the document; the next tick restores the layer. */ }
    finally { overlayBusy = false; }
  };
  const waitUntil = async (seconds) => {
    const delay = (seconds - elapsed()) * 1000;
    if (delay > 0) await sleep(delay);
  };
  const endSegment = (at) => {
    if (openSegment && at - openSegment.audio_start > 0.00001) segments.push({ ...openSegment, source_wall_end: openSegment.source_wall_start + at - openSegment.audio_start, audio_end: at, seconds: at - openSegment.audio_start, actualSourceStopWallAt: rawWallAt() });
    openSegment = null;
    chapterStarted = null;
    frozenAudioAt = at;
  };
  const beginSegment = async (scene, at) => {
    await browserAudioClock(page, at, false);
    openSegment = { scene: scene.id, source_wall_start: rawWallAt(), audio_start: at };
    frozenAudioAt = at;
    chapterStarted = performance.now();
  };
  const perform = async (scene, action, plannedAt) => {
    if (!preflight) await waitUntil(plannedAt);
    else previewAt = plannedAt;
    const began = elapsed();
    if (!preflight && began - plannedAt > schedule.tolerance) throw new Error(`${scene.id}: ${action.type} started late by${(began - plannedAt).toFixed(3)}s; fix the choreography before publication.`);
    const preparationBegan = rawWallAt();
    const cutPreparation = plan.choreography.cutViewPreparation === true;
    if (!preflight && cutPreparation) endSegment(plannedAt);
    if (cutPreparation || preflight) await browserAudioClock(page, plannedAt, true);
    const evidence = await usePage(page, { ...action, audioExpiresAt: action.duration ? plannedAt + action.duration : undefined }, { scene, caption: captions.find((c) => c.start <= elapsed() && elapsed() < c.end)?.text || '' });
    const completed = elapsed();
    const deadline = scene.offset + (action.readyBy ?? action.at + schedule.tolerance);
    if (!preflight && completed > deadline) throw new Error(`${scene.id}: ${action.type} completed at${completed.toFixed(3)}s after its${deadline.toFixed(3)}s visual deadline.`);
    lastOverlay = null;
    await refreshOverlay();
    if (cutPreparation || preflight) await browserAudioClock(page, plannedAt, true);
    if (!preflight && cutPreparation) {
      await page.evaluate(() => new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve))));
      await beginSegment(scene, plannedAt);
    }
    journal.push({ phase: cutPreparation ? 'cut_view_preparation' : 'audio', scene: scene.id, action, plannedAt, startedAt: began, completedAt: completed, elapsed: completed, sourceWallAt: rawWallAt(), preparationSeconds: rawWallAt() - preparationBegan, preparationExcluded: cutPreparation, latenessSeconds: preflight ? null : completed - plannedAt, url: page.url(), ...evidence });
  };
  try {
    // Warm actual read-only routes. Chapter setup is also excluded from the continuous narration.
    if (plan.choreography.warmRoutes !== false) {
      const routes = new Map();
      for (const scene of schedule.scenes) {
        routes.set(scene.path, { type: 'goto', path: scene.path, expect: scene.expect });
        for (const action of [...scene.setup, ...scene.actions]) if (action.type === 'goto') routes.set(action.path, action);
      }
      for (const action of routes.values()) await usePage(page, action);
    }
    if (!preflight) timer = setInterval(() => { if (chapterStarted !== null) refreshOverlay(); }, 40);
    for (const scene of schedule.scenes) {
      frozenAudioAt = scene.offset;
      previewAt = scene.offset;
      activeScene = scene;
      lastOverlay = null;
      await usePage(page, { type: 'clear_visuals' });
      await browserAudioClock(page, scene.offset, true);
      const initial = { type: 'goto', path: scene.path, expect: scene.expect };
      const setupStart = rawWallAt();
      if (page.url() !== new URL(scene.path, base).href) await usePage(page, initial);
      journal.push({ phase: 'setup', scene: scene.id, action: initial, audioClockFrozenAt: scene.offset, sourceWallAt: rawWallAt(), url: page.url() });
      await refreshOverlay();
      for (const action of scene.setup) {
        const evidence = await usePage(page, action, { scene, caption: captions.find((c) => c.start <= scene.offset && scene.offset < c.end)?.text || '' });
        journal.push({ phase: 'setup', scene: scene.id, action, audioClockFrozenAt: scene.offset, sourceWallAt: rawWallAt(), url: page.url(), ...evidence });
      }
      await refreshOverlay();
      await fs.writeFile(path.join(rawDir, `${scene.id}-snapshot.txt`), await page.locator('body').innerText());
      await page.screenshot({ path: path.join(rawDir, `${scene.id}-start.png`) });
      await page.evaluate(() => new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve))));
      if (!preflight) {
        recordedAt ??= new Date().toISOString();
        await beginSegment(scene, scene.offset);
      }
      journal.push({ phase: 'chapter_ready', scene: scene.id, audioAt: scene.offset, sourceWallAt: rawWallAt(), excludedSetupSeconds: rawWallAt() - setupStart, url: page.url() });
      for (const action of scene.actions) {
        await perform(scene, action, scene.offset + action.at);
        if (preflight && ['highlight', 'explain'].includes(action.type)) await page.screenshot({ path: path.join(rawDir, `${scene.id}-${action.at.toFixed(3)}-${action.type}.png`) });
      }
      if (!preflight) {
        const end = scene.offset + scene.seconds;
        if (elapsed() - end > schedule.tolerance) throw new Error(`${scene.id} exceeded its audio chapter; review latency and re-record.`);
        await waitUntil(end);
        endSegment(end);
        await browserAudioClock(page, end, true);
      }
      await page.screenshot({ path: path.join(rawDir, `${scene.id}-end.png`) });
      console.log(JSON.stringify({ chapter: scene.id, finished: true, recordedSeconds: preflight ? null : scene.offset + scene.seconds }));
    }
  } finally {
    if (timer) clearInterval(timer);
    await fs.writeFile(path.join(rawDir, 'journal.json'), JSON.stringify({ clock: 'audio', setupExcluded: true, recordedAt, audioSha256: audioHash, captionsSha256: captionsHash, segments, journal, pageErrors: errors }, null, 2) + '\n');
    const video = page.video();
    rawClockStopped = rawWallAt();
    await context.close();
    if (video && !preflight) await video.saveAs(path.join(rawDir, 'browser.webm'));
    await browser.close();
  }
  if (errors.length) throw new Error(`Browser page errors: ${errors.join('; ')}`);
  if (preflight) {
    console.log(JSON.stringify({ preflight: 'passed', snapshots: rawDir }));
    return;
  }
  await approvedPreparedAudio(plan);
  if (await fileHash(path.join(output, 'captions.json')) !== captionsHash) throw new Error('Canonical captions changed during capture; do not publish this recording.');
  const rawVideo = path.join(rawDir, 'browser.webm');
  const rawSeconds = duration(rawVideo);
  // Capture and export are separate. Browser startup and teardown are not video-clock anchors.
  // Publication requires independently measured visual calibration and one post-rendered footer.
  const captureMetadata = {
    schema: 'nerudo.submission-capture.v2', recordedAt, capturedAt: new Date().toISOString(),
    exportStatus: 'requires_measured_visual_calibration', freshBrowsing: true, raw: rawDir,
    rawVideoSha256: await fileHash(rawVideo), journalSha256: await fileHash(path.join(rawDir, 'journal.json')),
    sourceScriptSha256: await fileHash(fileURLToPath(import.meta.url)), planSha256: await fileHash(configPath),
    audioSha256: audioHash, captionsSha256: captionsHash, sourceSeconds: rawSeconds,
    rawRecorderStopWallSeconds: rawClockStopped, audioTimelineSeconds: schedule.seconds,
    publicOrigin: base.origin, allowedPublicOrigins: [...allowedPublicOrigins], experience: plan.experience || null,
    deployment: plan.deployment ?? null, providerEvidence: plan.provider_evidence ?? null,
    narrationProvenance: plan.narration_provenance, disclosure: plan.disclosure,
    note: 'Capture only. Do not derive origin from container duration or publish the native browser footer. Use --resume-capture with independent visual calibration and the preserved capture script.'
  };
  await fs.writeFile(path.join(rawDir, 'capture-metadata.json'), JSON.stringify(captureMetadata, null, 2) + '\n');
  console.log(JSON.stringify({ captured: true, exportStatus: captureMetadata.exportStatus, raw: rawDir, rawSeconds, audioTimelineSeconds: schedule.seconds, rawVideoSha256: captureMetadata.rawVideoSha256 }));
}

function chapterMap(plan) {
  let sourceAt = 0;
  let renderAt = 0;
  return plan.scenes.map((scene) => {
    const sourceSeconds = scene.source_scene_seconds ?? scene.seconds;
    const mapping = { id: scene.id, source_start: sourceAt, source_end: sourceAt + sourceSeconds, render_start: renderAt, render_end: renderAt + scene.seconds, scale: scene.seconds / sourceSeconds };
    sourceAt += sourceSeconds;
    renderAt += scene.seconds;
    return mapping;
  });
}

function audioRecordingLayers(plan, captions, journal) {
  const schedule = choreography(plan, captions);
  const origins = publicOrigins(plan);
  const events = journal.journal.filter((e) => ['chapter_ready', 'cut_view_preparation', 'audio'].includes(e.phase)).map((e) => ({ at: e.phase === 'chapter_ready' ? e.audioAt : e.plannedAt, url: e.url }));
  for (const event of events) {
    if (!Number.isFinite(event.at) || event.at < 0 || event.at > schedule.seconds) throw new Error('Caption event must have a finite time inside the approved audio timeline.');
    const url = new URL(event.url);
    if (!origins.has(url.origin) || url.search || url.hash || url.username || url.password) throw new Error('Caption URL must remain an anonymous, explicitly allowed public origin.');
  }
  events.sort((a, b) => a.at - b.at);
  const points = [...new Set([0, schedule.seconds, ...schedule.scenes.flatMap((s) => [s.offset, s.offset + s.seconds]), ...captions.flatMap((c) => [c.start, c.end]), ...events.map((e) => e.at)])].filter((n) => Number.isFinite(n) && n >= 0 && n <= schedule.seconds).sort((a, b) => a - b);
  return points.slice(0, -1).map((start, i) => {
    const end = points[i + 1]; const at = (start + end) / 2;
    const scene = schedule.scenes.find((s) => s.offset <= at && at < s.offset + s.seconds);
    return { start, end, title: scene.title, label: scene.label, url: events.findLast((e) => e.at <= at)?.url || new URL(scene.path, base).href, caption: captions.find((c) => c.start <= at && at < c.end)?.text || '' };
  });
}

async function resumeCapture() {
  const rawDir = path.resolve(arg('resume-capture', ''));
  const rawVideo = path.join(rawDir, 'browser.webm');
  const journalFile = path.join(rawDir, 'journal.json');
  const calibrationFile = arg('calibration', null);
  const captureScript = arg('capture-script', null);
  if (!calibrationFile || !captureScript) throw new Error('Recovery requires independent visual --calibration and the preserved original --capture-script; the raw end duration is not a start-clock anchor.');
  const plan = JSON.parse(await fs.readFile(configPath, 'utf8'));
  const captions = JSON.parse(await fs.readFile(path.join(output, 'captions.json'), 'utf8'));
  const schedule = choreography(plan, captions);
  const journal = JSON.parse(await fs.readFile(journalFile, 'utf8'));
  const calibration = JSON.parse(await fs.readFile(calibrationFile, 'utf8'));
  const bounds = calibration.statistics?.frameAwareCommonPermissibleOffset;
  const offset = calibration.statistics?.recommendedConstantOffsetSeconds;
  const rawHash = await fileHash(rawVideo); const journalHash = await fileHash(journalFile);
  const planHash = await fileHash(configPath); const captureScriptHash = await fileHash(captureScript);
  if (plan.publicOrigin !== base.origin) throw new Error('Capture plan origin differs from --base.');
  const finiteInterval = (interval) => Array.isArray(interval) && interval.length === 2 && interval.every(Number.isFinite) && interval[0] <= interval[1];
  if (!Array.isArray(calibration.anchors) || calibration.anchors.length < 3 || !bounds || !Number.isFinite(bounds.lowerSeconds) || !Number.isFinite(bounds.upperSeconds) || bounds.lowerSeconds > bounds.upperSeconds || bounds.nonempty !== true || !calibration.anchors.every((a) => finiteInterval(a.permissibleOffsetWithFrameCarrySeconds))) throw new Error('Visual calibration requires at least three finite ordered anchor intervals and a finite nonempty common interval.');
  const computedCommon = { lowerSeconds: Math.max(...calibration.anchors.map((a) => a.permissibleOffsetWithFrameCarrySeconds[0])), upperSeconds: Math.min(...calibration.anchors.map((a) => a.permissibleOffsetWithFrameCarrySeconds[1])) };
  if (computedCommon.lowerSeconds > computedCommon.upperSeconds || !Number.isFinite(offset) || offset < computedCommon.lowerSeconds || offset > computedCommon.upperSeconds || Math.abs(bounds.lowerSeconds - computedCommon.lowerSeconds) > 0.000001 || Math.abs(bounds.upperSeconds - computedCommon.upperSeconds) > 0.000001) throw new Error('Reported common calibration interval must equal the computed finite anchor intersection and contain the selected constant origin.');
  const captureMetadataFile = path.join(rawDir, 'capture-metadata.json');
  let captureMetadata = null;
  try { captureMetadata = JSON.parse(await fs.readFile(captureMetadataFile, 'utf8')); }
  catch (error) { if (error.code !== 'ENOENT') throw error; }
  let captureBinding;
  if (captureMetadata) {
    if (captureMetadata.schema !== 'nerudo.submission-capture.v2' || captureMetadata.exportStatus !== 'requires_measured_visual_calibration' || captureMetadata.planSha256 !== planHash || captureMetadata.sourceScriptSha256 !== captureScriptHash || captureMetadata.rawVideoSha256 !== rawHash || captureMetadata.journalSha256 !== journalHash || captureMetadata.audioSha256 !== journal.audioSha256 || captureMetadata.captionsSha256 !== journal.captionsSha256 || !Number.isFinite(captureMetadata.audioTimelineSeconds) || Math.abs(captureMetadata.audioTimelineSeconds - schedule.seconds) > 0.000001) throw new Error('Capture metadata does not bind this exact plan, preserved capture script, raw video, journal, narration, captions, and timeline.');
    captureBinding = { mode: 'capture-metadata', captureMetadataSha256: await fileHash(captureMetadataFile), planSha256: planHash, captureScriptSha256: captureScriptHash };
  } else {
    const legacyPlanHash = arg('legacy-capture-plan-sha256', null); const legacyScriptHash = arg('legacy-capture-script-sha256', null);
    if (!/^[a-f0-9]{64}$/.test(legacyPlanHash || '') || !/^[a-f0-9]{64}$/.test(legacyScriptHash || '') || legacyPlanHash !== planHash || legacyScriptHash !== captureScriptHash) throw new Error('Legacy capture has no immutable capture metadata. Supply independently verified --legacy-capture-plan-sha256 and --legacy-capture-script-sha256 matching its actual original plan and preserved script; no implicit legacy fallback.');
    captureBinding = { mode: 'explicit-legacy-hash-binding', planSha256: legacyPlanHash, captureScriptSha256: legacyScriptHash, limitation: 'Legacy raw journal did not attest these hashes; caller must verify original capture provenance independently.' };
  }
  if (calibration.source?.rawVideoSha256 !== rawHash || calibration.source?.journalSha256 !== journalHash || calibration.anchors?.length < 3 || !bounds?.nonempty || !Number.isFinite(offset) || offset < bounds.lowerSeconds || offset > bounds.upperSeconds || !calibration.statistics.recommendedOffsetPermittedByAllAnchors || calibration.anchors.some((a) => !Array.isArray(a.permissibleOffsetWithFrameCarrySeconds) || offset < a.permissibleOffsetWithFrameCarrySeconds[0] || offset > a.permissibleOffsetWithFrameCarrySeconds[1])) throw new Error('Visual calibration must bind this exact raw capture/journal and support the selected constant origin across its measured anchors.');
  if (journal.pageErrors?.length || !Array.isArray(journal.segments) || !journal.segments.length || !Array.isArray(journal.journal)) throw new Error('Successful capture journal and ready source segments required.');
  let previousAudioEnd = 0; let previousWallEnd = -Infinity;
  for (const segment of journal.segments) {
    if (![segment.audio_start, segment.audio_end, segment.source_wall_start, segment.source_wall_end].every(Number.isFinite) || segment.audio_end <= segment.audio_start || segment.source_wall_start < 0 || segment.source_wall_end <= segment.source_wall_start || Math.abs(segment.audio_start - previousAudioEnd) > 0.000001 || segment.source_wall_start < previousWallEnd - 0.000001 || Math.abs((segment.audio_end - segment.audio_start) - (segment.source_wall_end - segment.source_wall_start)) > 0.000001) throw new Error('Ready source segments must be finite, ordered, disjoint, and cover the continuous audio timeline without scaling or gaps.');
    previousAudioEnd = segment.audio_end; previousWallEnd = segment.source_wall_end;
  }
  if (Math.abs(previousAudioEnd - schedule.seconds) > 0.000001) throw new Error('Ready source segments do not reach the approved narration end.');
  // Validate events now, before any reuse or encoding can mutate output artifacts.
  const recordingLayers = audioRecordingLayers(plan, captions, journal);
  if (journal.journal.some((e) => e.target && e.target.captionTop !== dimensions.height - 115)) throw new Error('A fixed115px replacement would cover product content; inspect the actual recorded caption boundary.');
  await approvedPreparedAudio(plan);
  const audioHash = await fileHash(path.join(output, 'narration.wav')); const captionsHash = await fileHash(path.join(output, 'captions.json'));
  if (audioHash !== journal.audioSha256 || captionsHash !== journal.captionsSha256) throw new Error('Captured approved narration/captions changed; recovery never regenerates audio.');
  const rawSeconds = duration(rawVideo);
  const segments = readyFrameSegments(journal.segments.map((s) => ({ ...s, source_start: s.source_wall_start + offset, source_end: s.source_wall_end + offset })));
  if (segments.some((s) => s.source_start < 0 || s.source_end > rawSeconds) || segments.reduce((n, s) => n + s.encoded_frames, 0) !== Math.round(schedule.seconds * 25)) throw new Error('Calibrated ready segments must fit this raw capture and cover the approved audio frames exactly.');
  const segmentFile = path.join(rawDir, 'chapter-segments.json');
  let cutVideo;
  const readyCutVideo = arg('ready-cut-video', null);
  if (flag('check-resume-inputs') && !readyCutVideo) throw new Error('--check-resume-inputs requires an already calibrated --ready-cut-video; validation never encodes footage.');
  if (readyCutVideo) {
    // Reuse an already calibrated cut without rewriting its map or duplicating source encoding.
    cutVideo = path.resolve(readyCutVideo);
    const existing = JSON.parse(await fs.readFile(segmentFile, 'utf8'));
    if (existing.rawClockCorrectionSeconds !== offset || Math.abs(existing.rawSeconds - rawSeconds) > 0.000001 || existing.segments?.length !== segments.length) throw new Error('Existing ready-cut map does not match this measured calibration.');
    for (let i = 0; i < segments.length; i++) {
      for (const key of ['source_start', 'source_end', 'audio_start', 'audio_end', 'encoded_audio_start', 'encoded_audio_end', 'encoded_frames']) {
        if (!Number.isFinite(existing.segments[i][key]) || Math.abs(existing.segments[i][key] - segments[i][key]) > 0.000001) throw new Error(`Existing ready-cut segment${i} disagrees with calibrated ${key}.`);
      }
    }
    const expectedCutHash = existing.readyCutVideoSha256 || arg('legacy-ready-cut-sha256', null);
    if (!/^[a-f0-9]{64}$/.test(expectedCutHash || '') || expectedCutHash !== await fileHash(cutVideo)) throw new Error('Ready-cut map must bind the exact video SHA256. An older unhashed map requires independently verified --legacy-ready-cut-sha256; no implicit reuse fallback.');
    const cutProbe = JSON.parse(run('ffprobe', ['-v', 'error', '-show_format', '-show_streams', '-of', 'json', cutVideo]));
    const video = cutProbe.streams.find((stream) => stream.codec_type === 'video');
    if (video?.avg_frame_rate !== '25/1' || Number(video?.nb_frames) !== Math.round(schedule.seconds * 25) || Math.abs(Number(cutProbe.format.duration) - schedule.seconds) > 0.01) throw new Error('Existing ready-cut must contain exactly the approved25fps audio timeline.');
  } else {
    await fs.writeFile(segmentFile, JSON.stringify({ rawClockCorrectionSeconds: offset, calibrationSha256: await fileHash(calibrationFile), rawSeconds, segments }, null, 2) + '\n');
    const concatList = await encodeReadySegments(rawVideo, segments, path.join(rawDir, 'ready-clips'));
    cutVideo = path.join(rawDir, 'recovered-ready-video.mp4');
    run('ffmpeg', ['-hide_banner', '-loglevel', 'error', '-y', '-f', 'concat', '-safe', '0', '-i', concatList, '-c:v', 'copy', cutVideo]);
    await fs.writeFile(segmentFile, JSON.stringify({ rawClockCorrectionSeconds: offset, calibrationSha256: await fileHash(calibrationFile), rawVideoSha256: rawHash, journalSha256: journalHash, readyCutVideoSha256: await fileHash(cutVideo), rawSeconds, segments }, null, 2) + '\n');
  }
  if (flag('check-resume-inputs')) {
    console.log(JSON.stringify({ validated: true, captureBinding, readyCutVideoSha256: await fileHash(cutVideo), measuredOriginOffsetSeconds: offset, visualAnchors: calibration.anchors.length, readySegments: segments.length, audioTimelineSeconds: schedule.seconds, audioSha256: audioHash, captionsSha256: captionsHash }));
    return;
  }
  const staging = path.join(rawDir, 'single-caption-footer'); await fs.mkdir(staging, { recursive: true });
  const layersFile = path.join(staging, 'layers.json'); const mapFile = path.join(staging, 'identity-map.json');
  await fs.writeFile(layersFile, JSON.stringify(recordingLayers, null, 2) + '\n');
  await fs.writeFile(mapFile, JSON.stringify([{ id: 'ready-capture', source_start: 0, source_end: schedule.seconds, render_start: 0, render_end: schedule.seconds, scale: 1 }], null, 2) + '\n');
  const helper = fileURLToPath(new URL('./render_submission_captions.py', import.meta.url));
  const footer = JSON.parse(run(arg('caption-python', 'python3'), [helper, '--video', cutVideo, '--layers', layersFile, '--chapter-map', mapFile, '--fixed-band-height', '115', '--output', staging]));
  await new Promise((resolve, reject) => {
    const process = spawn('ffmpeg', ['-hide_banner', '-loglevel', 'warning', '-y', '-i', cutVideo, '-f', 'concat', '-safe', '0', '-i', path.join(staging, 'concat.txt'), '-i', path.join(output, 'narration.wav'), '-filter_complex', '[1:v]fps=25,format=rgba[footer];[0:v][footer]overlay=x=0:y=H-200:eof_action=repeat:format=auto[v]', '-map', '[v]', '-map', '2:a:0', '-t', String(schedule.seconds), '-fps_mode', 'cfr', '-c:v', 'libx264', '-preset', 'fast', '-crf', '21', '-pix_fmt', 'yuv420p', '-c:a', 'aac', '-b:a', '256k', '-movflags', '+faststart', videoPath], { stdio: 'inherit' });
    process.on('error', reject); process.on('exit', (code) => code === 0 ? resolve() : reject(new Error(`Recovered caption export failed (${code})`)));
  });
  await approvedPreparedAudio(plan);
  if (await fileHash(path.join(output, 'captions.json')) !== captionsHash) throw new Error('Caption input changed during recovery.');
  const probe = JSON.parse(run('ffprobe', ['-v', 'error', '-show_format', '-show_streams', '-of', 'json', videoPath])); const stat = await fs.stat(videoPath);
  if (probe.streams.find((s) => s.codec_type === 'video')?.avg_frame_rate !== '25/1' || Math.abs(Number(probe.format.duration) - schedule.seconds) > 0.05 || stat.size >= 2 * 1024 ** 3) throw new Error('Recovered export must retain25fps/exact approved duration and stay below2GiB.');
  const finalLoudness = loudnessStats(videoPath); const finalSilences = detectedSilences(videoPath, maxSilenceSeconds, -45);
  if (Math.abs(Number(finalLoudness.input_i) + 16) > 1 || Number(finalLoudness.input_tp) > -1.5 || finalSilences.length) throw new Error('Recovered audio failed encoded loudness/peak/silence gates.');
  const metadata = { recordedAt: journal.recordedAt, renderedAt: new Date().toISOString(), freshBrowsing: true, recoveredExport: true, file: videoPath, bytes: stat.size, sha256: await fileHash(videoPath), raw: rawDir, rawVideoSha256: rawHash, journalSha256: journalHash, captureSourceScriptSha256: await fileHash(captureScript), sourceScriptSha256: await fileHash(fileURLToPath(import.meta.url)), captionHelperSha256: await fileHash(helper), captureBinding, readyCutVideoSha256: await fileHash(cutVideo), publicOrigin: plan.publicOrigin, allowedPublicOrigins: [...publicOrigins(plan)], experience: plan.experience, deployment: plan.deployment, providerEvidence: plan.provider_evidence, disclosure: plan.disclosure, narrationProvenance: plan.narration_provenance, captionsSha256: captionsHash, planSha256: await fileHash(configPath), chapterSegmentsSha256: await fileHash(segmentFile), chapters: plan.scenes.map(({ id, title, seconds, label, path }) => ({ id, title, seconds, label, path })), captionReplacement: footer, choreography: { ...plan.choreography, setupExcluded: true, rawClockCorrectionSeconds: offset, sourceSeconds: rawSeconds, audioTimelineSeconds: schedule.seconds, segments, maxCueFrameRoundingSeconds: 0.02, timingMethod: `Measured constant raw origin from${calibration.anchors.length} independently observed visual onsets; ready cuts only, no duration scaling. Logical audio/caption clock unchanged.` }, recovery: { originalExportFailure: flag('recover-failed-export') ? 'End-duration-derived origin guard rejected capture; teardown tail is not a valid start-clock anchor.' : null, originalFailureSourceScriptSha256: await fileHash(captureScript), calibrationSha256: await fileHash(calibrationFile), calibrationBoundsSeconds: bounds, anchors: calibration.anchors, residualStatistics: calibration.statistics, footerReason: 'Chromium screencast gray padding clipped the native footer. Entire measured115px footer replaced once; real product pixels abovey885 preserved, subject to encoding.', publicationRequiresActualCutFrameReview: true }, finalLoudness, finalLongSilences: finalSilences, ffprobe: probe };
  await fs.writeFile(path.resolve(arg('metadata-output', path.join(output, 'visual-recording-metadata.json'))), JSON.stringify(metadata, null, 2) + '\n');
  console.log(JSON.stringify({ recovered: true, file: videoPath, seconds: schedule.seconds, bytes: stat.size, sha256: metadata.sha256, measuredOriginOffsetSeconds: offset, audioSha256: audioHash }));
}

function sourceToRender(seconds, mapping) {
  const chapter = mapping.find((m) => m.source_start <= seconds && seconds < m.source_end) || mapping.at(-1);
  return chapter.render_start + (seconds - chapter.source_start) * chapter.scale;
}

function recordingLayers(plan, captions, journal) {
  const mapping = chapterMap(plan);
  const scenes = plan.scenes.map((scene, i) => ({ ...scene, start: mapping[i].render_start, end: mapping[i].render_end }));
  const total = mapping.at(-1).render_end;
  const events = (journal.journal || []).map((e) => ({ ...e, elapsed: sourceToRender(e.elapsed, mapping) }));
  for (const event of events) {
    const url = new URL(event.url);
    if (url.origin !== base.origin || url.search) throw new Error('Retained journal must contain anonymous same-origin URLs without query keys.');
  }
  const points = [...new Set([0, total, ...scenes.flatMap((s) => [s.start, s.end]), ...captions.flatMap((c) => [c.start, c.end]), ...events.map((e) => e.elapsed).filter((t) => t > 0 && t < total)])].sort((a, b) => a - b);
  return points.slice(0, -1).map((start, i) => {
    const end = points[i + 1];
    const at = (start + end) / 2;
    const scene = scenes.find((s) => s.start <= at && at < s.end);
    const caption = captions.find((c) => c.start <= at && at < c.end)?.text || '';
    const event = events.findLast((e) => e.elapsed <= at);
    return { start, end, title: scene.title, label: scene.label, url: event?.url || new URL(scene.path || '/', base).href, caption };
  });
}

async function renarrate() {
  const rawVideo = arg('raw-video', null);
  if (!rawVideo) throw new Error('RETAINED_VIDEO_REQUIRED: pass --raw-video. Renarration never launches a browser or recaptures product footage.');
  const raw = path.resolve(rawVideo);
  const journalFile = path.resolve(arg('journal', path.join(path.dirname(raw), 'journal.json')));
  const journal = JSON.parse(await fs.readFile(journalFile, 'utf8'));
  if (!Array.isArray(journal.journal) || journal.pageErrors?.length) throw new Error('A successful original capture journal is required.');
  const originalMetadataFile = path.resolve(arg('capture-metadata', path.join(output, 'video-metadata.json')));
  const originalMetadata = JSON.parse(await fs.readFile(originalMetadataFile, 'utf8'));
  const originalHash = await fileHash(raw);
  const originalDuration = duration(raw);
  if (Math.abs(originalDuration - 480) > 0.5) throw new Error('Retained footage must be the original eight-minute capture (within0.5s container rounding).');
  const { plan, captions, staging } = await prepare();
  const mapping = chapterMap(plan);
  const renderDuration = mapping.at(-1).render_end;
  const mapFile = path.join(staging, 'chapter-map.json');
  await fs.writeFile(mapFile, JSON.stringify(mapping, null, 2) + '\n');
  const layersFile = path.join(staging, 'layers.json');
  await fs.writeFile(layersFile, JSON.stringify(recordingLayers(plan, captions, journal), null, 2) + '\n');
  const overlayDirectory = path.join(staging, 'overlays');
  const python = arg('caption-python', 'python3');
  const helper = fileURLToPath(new URL('./render_submission_captions.py', import.meta.url));
  const helperArgs = [helper, '--video', raw, '--layers', layersFile, '--output', overlayDirectory, '--chapter-map', mapFile];
  if (arg('caption-font', null)) helperArgs.push('--font', arg('caption-font'));
  if (arg('caption-bold-font', null)) helperArgs.push('--bold-font', arg('caption-bold-font'));
  let overlay;
  try { overlay = JSON.parse(run(python, helperArgs)); }
  catch (error) { throw new Error(`Caption-layer rendering requires an existing Python runtime with Pillow and readable fonts; no browser capture or dependency installation is attempted. ${error.message}`); }
  let retiming = '(PTS*TB)';
  for (const m of [...mapping].reverse()) {
    const mapped = `(${m.render_start}+(PTS*TB-${m.source_start})*${m.scale})`;
    retiming = `if(lt(PTS*TB,${m.source_end}),${mapped},${retiming})`;
  }
  await approvedPreparedAudio(plan);
  const filter = `[0:v]tpad=stop_mode=clone:stop_duration=${Math.max(0, 480 - originalDuration)},trim=duration=480,setpts='${retiming}/TB',fps=25[base];[1:v]fps=25,format=rgba[caption];[base][caption]overlay=x=0:y=H-200:eof_action=repeat:format=auto[v]`;
  await new Promise((resolve, reject) => {
    const process = spawn('ffmpeg', ['-hide_banner', '-loglevel', 'warning', '-y', '-i', raw, '-f', 'concat', '-safe', '0', '-i', path.join(overlayDirectory, 'concat.txt'), '-i', path.join(output, 'narration.wav'), '-filter_complex', filter, '-map', '[v]', '-map', '2:a:0', '-t', String(renderDuration), '-fps_mode', 'cfr', '-c:v', 'libx264', '-preset', 'fast', '-crf', '21', '-pix_fmt', 'yuv420p', '-c:a', 'aac', '-b:a', '256k', '-movflags', '+faststart', videoPath], { stdio: 'inherit' });
    process.on('error', reject); process.on('exit', (code) => code === 0 ? resolve() : reject(new Error(`Renarration encoding failed (${code})`)));
  });
  const probe = JSON.parse(run('ffprobe', ['-v', 'error', '-show_format', '-show_streams', '-of', 'json', videoPath]));
  const stat = await fs.stat(videoPath);
  if (Math.abs(Number(probe.format.duration) - renderDuration) > 0.1 || stat.size >= 2 * 1024 ** 3) throw new Error('Renarrated video must match the natural narration duration and remain below2GiB.');
  await approvedPreparedAudio(plan);
  const finalLoudness = loudnessStats(videoPath);
  if (Math.abs(Number(finalLoudness.input_i) + 16) > 1 || Number(finalLoudness.input_tp) > -1.5) throw new Error('Encoded audio does not meet the loudness/peak target.');
  const finalSilences = detectedSilences(videoPath, maxSilenceSeconds, -45);
  if (finalSilences.length) throw new Error('Encoded narration contains a long silence; do not publish this render.');
  const metadata = {
    schema: 'nerudo.submission-renarration.v1', renderedAt: new Date().toISOString(),
    file: videoPath, bytes: stat.size, sha256: await fileHash(videoPath), ffprobe: probe,
    capture: { reused: true, freshBrowsing: false, recordedAt: originalMetadata.recordedAt || originalMetadata.recorded_at, originalVideoSha256: originalHash, originalJournalSha256: await fileHash(journalFile), originalMetadataSha256: await fileHash(originalMetadataFile), source: originalMetadata.deployment || null, originalDurationSeconds: originalDuration },
    narrator: plan.narration_provenance, captionReplacement: overlay, chapterRetiming: mapping,
    visualScope: 'Each source chapter retimed to natural unaltered narration; no chapter or action omitted. Existing recording-overlay band replaced. Original product footage above each detected band retained, subject to lossy re-encoding; no whole-frame pixel identity claim.',
    scriptSha256: await fileHash(fileURLToPath(import.meta.url)), captionHelperSha256: await fileHash(helper),
    planSha256: await fileHash(configPath), captionsSha256: await fileHash(path.join(output, 'captions.json')),
    finalLoudness, finalLongSilences: finalSilences, disclosure: plan.disclosure, publicOrigin: base.origin,
    deployment: plan.deployment || null, providerEvidence: plan.provider_evidence || null,
  };
  await fs.writeFile(path.join(output, 'renarration-metadata.json'), JSON.stringify(metadata, null, 2) + '\n');
  console.log(JSON.stringify({ renarrated: true, video: videoPath, seconds: Number(probe.format.duration), bytes: stat.size, sha256: metadata.sha256, provider: plan.narration_provenance.provider, retainedVideoSha256: originalHash }));
}

async function audioTemplate() {
  const plan = await readPlan();
  console.log(JSON.stringify({ schema: 'nerudo.narration.v1', approved: true, provider: { name: 'REQUIRED', model: 'REQUIRED', voice: 'REQUIRED', kind: 'neural', gender: 'female', style: 'natural conversational', quality: 'high' }, scenes: plan.scenes.map((s) => ({ id: s.id, file: `${s.id}.wav`, transcript_sha256: sha256(s.narration), source_scene_seconds: s.source_scene_seconds ?? s.seconds, narration: s.narration })) }, null, 2));
}

export { choreography, productTarget, targetEvidence, highlight, explanation, usePage, overlay, browserAudioClock, readyFrameSegments, encodeReadySegments };

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  if (flag('audio-template')) await audioTemplate();
  else if (flag('check-audio')) { const checked = await validatedAudio(await readPlan()); console.log(JSON.stringify({ valid: true, provider: checked.provider, clips: checked.inputs.map(({ scene, inputSeconds }) => ({ id: scene.id, inputSeconds })) })); }
  else if (flag('check-prepared-audio')) { await approvedPreparedAudio(await readPlan()); console.log(JSON.stringify({ approvedPreparedAudio: true })); }
  else if (flag('check-choreography')) { const plan = await readPlan(); publicOrigins(plan); const schedule = choreography(plan, JSON.parse(await fs.readFile(path.join(output, 'captions.json'), 'utf8'))); await approvedPreparedAudio(plan); console.log(JSON.stringify({ valid: true, clock: 'audio', seconds: schedule.seconds, actions: schedule.scenes.map((s) => ({ scene: s.id, actions: s.actions })) })); }
  else if (flag('resume-capture')) await resumeCapture();
  else if (flag('renarrate')) await renarrate();
  else if (flag('prepare')) await prepare();
  else if (flag('preflight')) await record(true);
  else if (flag('record')) await record(false);
  else console.log('Use --check-choreography, then --preflight/--record --ready --browser-channel chrome with an explicit audio-clock plan. --record saves capture only: --resume-capture requires independently measured visual origin calibration and emits one complete caption footer. Approved natural WAV and canonical captions remain unchanged. --renarrate is only for retained footage. No basic/system voice or fallback exists; nothing is installed.');
}
