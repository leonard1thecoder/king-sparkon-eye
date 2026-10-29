# ONNX Models & Rust Inference Contracts (Issue #58)

This document serves as the verified inventory and contract specification for ONNX inference models used in King Sparkon Eye, fulfilling the requirements of **Issue #58** and unblocking **Issue #59 (Face-Recognition Embedding Adapter)** and **Issue #30 (Gate Identification)**.

---

## 1. Inventory Summary

| Model Identifier | Purpose | Model Format / Runtime | Status in Repository |
|---|---|---|---|
| `yunet-2023mar` | 5-point landmark face detection | ONNX (FP32), ORT 1.31.1 | **Vendored & Verified** |
| `sface-2021dec` (Target Recognizer) | 128-D L2-normalized face recognition feature embeddings | ONNX (FP32), ORT 1.31.1 | **Contract Specified; Gated on Weights Vendoring** |

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
- **Not yet wired:** the detector is not loaded at startup or by readiness (`/health/ready` checks only the vision provider).

---

## 3. Face Recognizer Contract: `sface-2021dec`

This specification governs the adapter implementation in **Issue #59**:

- **Model Identifier:** `sface-2021dec`
- **Purpose:** Event-scoped 1:N face identification candidate similarity ranking.
- **License Requirement:** Permissive (MIT / Apache 2.0).
- **Input Tensor Contract:**
  - Name: `data`
  - Shape: `[1, 3, 112, 112]`
  - DataType: `float32`
  - Preprocessing: Face cropped and aligned using 5-point landmarks from YuNet detector, resized to 112x112, BGR channel order, pixel values normalized by subtracting 127.5 and multiplying by 0.0078125 (`(x - 127.5) / 128.0`).
- **Output Tensor Contract:**
  - Name: `embedding` (or `fc1`)
  - Shape: `[1, 128]`
  - DataType: `float32`
  - Normalization: $L_2$ norm ($||\mathbf{v}||_2 = 1.0$). If raw output is unnormalized, the Rust adapter must explicitly normalize and verify all values are finite (`!val.is_nan() && !val.is_infinite()`).
- **Distance / Metric:**
  - Cosine similarity: $\cos(\theta) = \mathbf{u} \cdot \mathbf{v}$.
  - Range: $[-1.0, 1.0]$.
  - Operational Guard: Candidate comparison is strictly scoped to the enrolled attendees of the authorized event; global cross-event matching is prohibited.

---

## 4. Rust Adapter Interface Rules (Issue #59 Prerequisite)

1. **Finite Output Checks:** The Rust adapter must reject any model inference output containing `NaN` or `+/-Infinity`.
2. **Dimension Verification:** The adapter must strictly assert that the returned vector length matches exactly 128 elements.
3. **No Fabricated Thresholds:** Calibrated similarity thresholds remain gated on **Issue #61** using consented, subject-disjoint evaluation fixtures (**Issue #55**).
4. **No Identity Proclamation:** Embedding comparison outputs a typed candidate match outcome (`MatchOutcome::LikelyMatch`, `MatchOutcome::Uncertain`, `MatchOutcome::LikelyMismatch`, `MatchOutcome::NoCandidate`), never an admission authorization.
