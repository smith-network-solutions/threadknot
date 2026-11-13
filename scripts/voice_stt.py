# Threadknot voice-parlay STT sidecar.
#
# A conversation cannot pay a fresh Python start plus a Whisper model load for
# every utterance the way per-clip dictation does, so this process loads
# faster-whisper once and then transcribes on request until killed. Spawned by
# src-tauri/src/voice/stt.rs, which embeds this file and writes it into the
# data dir at launch (so packaged builds carry it and dev edits still apply).
#
# Wire protocol, JSON per line:
#   stdin:   {"op": "transcribe", "id": "<echo>", "path": "<wav path>",
#             "beam": 5, "prompt": "<text spoken just before>"}
#            beam and prompt are optional. Live dictation asks for beam 1 on
#            its in-progress guesses (speed) and passes the words already on
#            screen as the prompt so casing and punctuation carry across pauses.
#   stdout:  {"ev": "ready", "device": "cuda"|"cpu", "model": "...",
#             "hint": "<why the GPU is not in use, when one exists>"}  (once)
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

# Every machine is different, so nothing here is assumed: the sidecar walks a
# ladder from the best setup to the most modest one and takes the first rung
# that survives a real transcription on this hardware.
#
#   1. NVIDIA GPU, large model, float16      (needs roughly 2.5 GB of free VRAM)
#   2. NVIDIA GPU, small model, int8_float16 (well under 1 GB, for small cards
#                                             or a card already full of other work)
#   3. CPU, int8, a model sized to the core count
#
# A machine with no NVIDIA GPU (AMD, Intel, Apple) goes straight to rung 3:
# CTranslate2 only accelerates on CUDA. Measured on an RTX 3060 with a 12 second
# clip: distil-large-v3 0.46 s, medium.en 0.70 s, small.en 0.37 s; small.en on
# the CPU of the same machine 1.8 s.
#
# THREADKNOT_VOICE_STT_MODEL pins one model for every rung.
# THREADKNOT_VOICE_STT_DEVICE=cpu skips the GPU (to leave it to other work).
GPU_MODEL = "distil-large-v3"
GPU_SMALL_MODEL = "small.en"
CORES = os.cpu_count() or 4
CPU_MODEL = "small.en" if CORES >= 8 else "base.en"
PINNED = os.environ.get("THREADKNOT_VOICE_STT_MODEL", "").strip()
FORCE_DEVICE = os.environ.get("THREADKNOT_VOICE_STT_DEVICE", "").strip().lower()


def language_for(name):
    """English-only models must be told so; multilingual ones detect."""
    forced = os.environ.get("THREADKNOT_VOICE_STT_LANGUAGE", "").strip().lower()
    if forced:
        return None if forced == "auto" else forced
    return "en" if name.endswith(".en") or name.startswith("distil-") else None


def prove(candidate, name):
    """Run a second of silence through the model end to end.

    Constructing a CUDA model succeeds even when cuBLAS or cuDNN is missing;
    the missing DLL only surfaces on the first real encode. Without this the
    sidecar would report itself ready on the GPU and then fail every single
    request, never reaching the rungs below. It also pays the one-time CUDA
    warm-up here, so the first words spoken are not the slow ones.
    """
    import numpy as np

    segments, _info = candidate.transcribe(
        np.zeros(16000, dtype=np.float32),
        beam_size=1,
        language=language_for(name) or "en",
        vad_filter=False,
        condition_on_previous_text=False,
    )
    for _ in segments:
        pass


def cuda_devices():
    try:
        import ctranslate2

        return ctranslate2.get_cuda_device_count()
    except Exception:  # noqa: BLE001 - no CUDA build, no driver: same answer
        return 0


HAS_GPU = cuda_devices() > 0
ladder = []
if HAS_GPU and FORCE_DEVICE != "cpu":
    ladder.append((PINNED or GPU_MODEL, "cuda", "float16"))
    ladder.append((PINNED or GPU_SMALL_MODEL, "cuda", "int8_float16"))
ladder.append((PINNED or CPU_MODEL, "cpu", "int8"))

model = None
device = "cpu"
MODEL = ""
gpu_error = ""
last_error = ""
for name, dev, compute in ladder:
    try:
        kwargs = {"cpu_threads": CORES} if dev == "cpu" else {}
        candidate = WhisperModel(name, device=dev, compute_type=compute, **kwargs)
        prove(candidate, name)
        model, device, MODEL = candidate, dev, name
        break
    except Exception as e:  # noqa: BLE001 - the next rung is the handling
        last_error = str(e)
        if dev == "cuda" and not gpu_error:
            gpu_error = str(e)

if model is None:
    emit({"ev": "fatal", "error": f"could not load a Whisper model: {last_error}"})
    sys.exit(1)

LANGUAGE = language_for(MODEL)

# Say why a GPU that exists is not the one doing the work, because the fix is
# usually one command and nothing else would ever mention it.
hint = ""
if HAS_GPU and device == "cpu" and FORCE_DEVICE != "cpu":
    lowered = gpu_error.lower()
    if "cublas" in lowered or "cudnn" in lowered or ".dll" in lowered or ".so" in lowered:
        hint = (
            "An NVIDIA GPU is present but its CUDA libraries are missing, so speech runs on the CPU. "
            "Install them with: pip install nvidia-cublas-cu12 nvidia-cudnn-cu12"
        )
    else:
        hint = f"An NVIDIA GPU is present but could not be used, so speech runs on the CPU: {gpu_error}"

emit({"ev": "ready", "device": device, "model": MODEL, "hint": hint})

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
        try:
            beam = max(1, min(10, int(req.get("beam") or 5)))
        except (TypeError, ValueError):
            beam = 5
        prompt = req.get("prompt")
        segments, _info = model.transcribe(
            req["path"],
            beam_size=beam,
            language=LANGUAGE,
            # Context for casing and punctuation only. It seeds the first
            # window and is never fed back, so it cannot start a loop.
            initial_prompt=prompt if isinstance(prompt, str) and prompt.strip() else None,
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
