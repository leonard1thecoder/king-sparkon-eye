# ONNX Models & Rust Inference Contracts (Issue #58)

This document serves as the verified inventory and contract specification for ONNX inference models used in King Sparkon Eye, fulfilling the requirements of **Issue #58** and unblocking **Issue #59 (Face-Recognition Embedding Adapter)** and **Issue #30 (Gate Identification)**.

---

## 1. Inventory Summary

| Model Identifier | Purpose | Model Format / Runtime | Status in Repository |
|---|---|---|---|
| `yunet-2023mar` | 5-point landmark face detection | ONNX (FP32), ORT 1.31.1 | **Vendored & Verified** |
| `sface-2021dec` | 128-D face-recognition embeddings | ONNX (FP32), ORT 1.31.1 | **Rust adapter implemented in PR; weights not vendored pending provenance review** |

---

## 2. Face Detector: `yunet-2023mar`

- **Artifact Path:** `artifacts/models/yunet-2023mar/face_detection_yunet_2023mar.onnx`
- **SHA-256 Digest:** `8f2383e4dd3cfbb4553ea8718107fc0423210dc964f9f4280604804ed2552fa4`
- **License:** MIT (`artifacts/models/yunet-2023mar/LICENSE`, license digest `c83b8120c50ccbd4c4f96edf53141bdd566ebb8f8e9227e415326aa1b1aba958`)
- **Upstream Source:** OpenCV Model Zoo (`libfacedetection.train` by Shiqi Yu)
- **Input Tensor Contract:**
  - Name: `input`
  - Shape: `[1, 3, 640, 640]`
  - DataType: `float32`
  - Preprocessing: BGR pixel order, normalized to `[0.0, 255.0]`, resized with letterbox preserving aspect ratio.
- **Output Tensor Groups:**
  - `cls_{8,16,32}`: Classification score maps (`[1, 6400, 1]`, `[1, 1600, 1]`, `[1, 400, 1]`).
  - `obj_{8,16,32}`: Objectness confidence maps (`[1, 6400, 1]`, `[1, 1600, 1]`, `[1, 400, 1]`).
  - `bbox_{8,16,32}`: Bounding box offsets (`[1, 6400, 4]`, `[1, 1600, 4]`, `[1, 400, 4]`).
  - `kps_{8,16,32}`: 5 landmark keypoints (`[1, 6400, 10]`, `[1, 1600, 10]`, `[1, 400, 10]`) covering right eye, left eye, nose tip, right mouth corner, left mouth corner.
- **Verification (`tests/yunet_test.rs`):** the vendored model loads and passes I/O validation; missing or corrupt artifacts return controlled errors; repeated calls and fresh instances return the same detections within `1e-6`. Real-face accuracy and zero/one/many fixtures on real photos are still open (#55).
- **Wired:** the detector is loaded when the router is built and used by `POST /v1/enrollment/photo-assessments`. `/health/ready` reports a `face_detector` dependency as `unavailable` only if the load failed.

---

## 3. Face Recognizer Contract: `sface-2021dec`

### Upstream artifact identity (not vendored)

- **Artifact:** `face_recognition_sface_2021dec.onnx`
- **Upstream repository:** [OpenCV Zoo SFace model directory](https://github.com/opencv/opencv_zoo/tree/main/models/face_recognition_sface)
- **Upstream-reported size:** 38,696,353 bytes.
- **Upstream-reported SHA-256:** `0ba9fbfa01b5270c96627c4ef784da859931e02f04419c829e83484087c34e79`.
- **Declared upstream license:** Apache-2.0 for files in the SFace model directory, per its [README](https://github.com/opencv/opencv_zoo/blob/main/models/face_recognition_sface/README.md).
- **Compliance status:** the exact checkpoint's training-data provenance and dataset-derived conditions have not been authoritatively clarified. OpenCV Zoo [issue #313](https://github.com/opencv/opencv_zoo/issues/313) remains open. Do not vendor, redistribute or enable the weights in production until the project's legal/compliance owner records an explicit decision. The directory license declaration and artifact checksum are not evidence that training-data rights have been resolved.
- **Runtime supply chain:** no automatic model download. The recognizer accepts an explicitly supplied local model path and verifies the exact expected byte size and SHA-256 before ORT parses it. The ONNX Runtime library is the existing pinned 1.31.1 CPU runtime shared with YuNet.

### Verified preprocessing and alignment contract

The adapter follows the upstream OpenCV `FaceRecognizerSF` implementation:

- Five landmarks are consumed in YuNet/OpenCV order: subject's right eye, subject's left eye, nose tip, subject's right mouth corner, subject's left mouth corner.
- `alignCrop` estimates a similarity transform to the canonical five-point template and warps to **112×112** using linear interpolation and constant-zero border behavior.
- The canonical target points are:
  - `(38.2946, 51.6963)`
  - `(73.5318, 51.5014)`
  - `(56.0252, 71.7366)`
  - `(41.5493, 92.3655)`
  - `(70.7299, 92.2041)`
- Feature extraction calls `blobFromImage(aligned, 1, Size(112,112), Scalar(0,0,0), true, false)`: FP32 NCHW, RGB channel order, raw pixel values `[0,255]`, no mean subtraction or scaling. Ingress images are already RGB, so the Rust adapter emits RGB planes directly.
- The ONNX graph is required at load time to expose exactly one input of shape `[1,3,112,112]` and one output of shape `[1,128]`. The adapter reads tensor names from graph metadata rather than hardcoding `data`, `embedding` or `fc1`; inference supplies FP32 input and extracts FP32 output, rejecting incompatible graph types.
- The extracted feature is validated as exactly 128 finite values and L2-normalized by the Rust `FaceEmbedding` type.

The preprocessing contract above is based on the upstream [OpenCV FaceRecognizerSF source](https://github.com/opencv/opencv/blob/4.x/modules/objdetect/src/face_recognize.cpp), not the earlier incorrect `(x - 127.5) / 128` description.

### Implementation boundary

- `SFaceRecognizer` is a library adapter only. It is not registered in HTTP handlers, API readiness, enrollment or the #187 worker runtime.
- `extract_from_detection` aligns the original normalized RGB image from a five-landmark YuNet detection and then runs the SFace session.
- Missing artifacts, invalid graph metadata, malformed inputs, non-FP32 outputs, unexpected shapes and non-finite embeddings fail with fixed typed errors.
- The adapter does not select a match threshold, decide identity, determine liveness or consent, or authorize a business action.
- #95 remains a hard runtime gate. No capture admission, template creation, processor registration or production inference activation is allowed before explicit evaluation approval.
- #61 owns accuracy evaluation, subgroup analysis, operating thresholds and rollout evidence. Synthetic tests validate transforms/tensor contracts only; they are not recognition-accuracy evidence.

## 4. Rust Adapter Interface Rules (Issue #59 Prerequisite)

1. **Finite Output Checks:** The Rust adapter must reject any model inference output containing `NaN` or `+/-Infinity`.
2. **Dimension Verification:** The adapter must strictly assert that the returned vector length matches exactly 128 elements.
3. **No Fabricated Thresholds:** Calibrated similarity thresholds remain gated on **Issue #61** using consented, subject-disjoint evaluation fixtures (**Issue #55**).
4. **No Identity Proclamation:** Embedding extraction and cosine similarity are candidate signals only. The authoritative typed processor outcomes remain defined by #186/#265; this adapter never authorizes a business action.
