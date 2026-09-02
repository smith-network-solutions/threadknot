# Threadknot voice-parlay STT sidecar.
#
# A conversation cannot pay a fresh Python start plus a Whisper model load for
# every utterance the way per-clip dictation does, so this process loads
# faster-whisper once and then transcribes on request until killed. Spawned by
# src-tauri/src/voice/stt.rs, which embeds this file and writes it into the
# data dir at launch (so packaged builds carry it and dev edits still apply).
#
# Wire protocol, JSON per line:
#   stdin:   {"op": "transcribe", "id": "<echo>", "path": "<wav path>"}
#   stdout:  {"ev": "ready", "device": "cuda"|"cpu", "model": "..."}   (once)
#            {"ev": "transcript", "id": "...", "text": "...", "ms": 412}
#            {"ev": "error", "id": "...", "error": "..."}              (non-fatal)
#            {"ev": "fatal", "error": "..."}                           (then exit)
import json
import os
import site
import sys
import time

# CUDA DLLs ship via pip wheels; Windows needs the dll dirs added explicitly
# or faster-whisper's CUDA backend silently fails to load.
for _sp in site.getsitepackages():
    for _sub in ("nvidia/cublas/bin", "nvidia/cudnn/bin", "nvidia/cuda_nvrtc/bin"):
        _d = os.path.join(_sp, *_sub.split("/"))
        if os.path.isdir(_d):
            os.add_dll_directory(_d)
            os.environ["PATH"] = _d + os.pathsep + os.environ["PATH"]


def emit(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()


try:
    from faster_whisper import WhisperModel
except Exception as e:  # noqa: BLE001 - anything here is fatal and reported
    emit({"ev": "fatal", "error": f"faster-whisper is not importable: {e}"})
    sys.exit(1)

MODEL = os.environ.get("THREADKNOT_VOICE_STT_MODEL", "medium.en").strip() or "medium.en"
LANGUAGE = "en" if MODEL.endswith(".en") else None

try:
    model = WhisperModel(MODEL, device="cuda", compute_type="float16")
    _ = model.model  # force init so CUDA errors surface here, not mid-stream
    device = "cuda"
except Exception:  # noqa: BLE001 - CPU fallback is the documented behavior
    try:
        model = WhisperModel(MODEL, device="cpu", compute_type="int8", cpu_threads=os.cpu_count() or 4)
        device = "cpu"
    except Exception as e:  # noqa: BLE001
        emit({"ev": "fatal", "error": f"could not load Whisper model {MODEL}: {e}"})
        sys.exit(1)

emit({"ev": "ready", "device": device, "model": MODEL})

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    try:
        req = json.loads(line)
    except ValueError:
        continue
    if req.get("op") != "transcribe":
        continue
    req_id = req.get("id", "")
    started = time.time()
    try:
        segments, _info = model.transcribe(
            req["path"],
            beam_size=5,
            language=LANGUAGE,
            vad_filter=True,
            vad_parameters={"min_silence_duration_ms": 500},
            # Each utterance stands alone; carried context makes Whisper loop.
            condition_on_previous_text=False,
        )
        text = " ".join(s.text.strip() for s in segments).strip()
        emit(
            {
                "ev": "transcript",
                "id": req_id,
                "text": text,
                "ms": int((time.time() - started) * 1000),
            }
        )
    except Exception as e:  # noqa: BLE001 - one bad clip must not kill the loop
        emit({"ev": "error", "id": req_id, "error": str(e)})
