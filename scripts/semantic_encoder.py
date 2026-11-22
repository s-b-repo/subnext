"""CPU encoder and derived vector index for Subnext's compact retrieval."""

import hashlib
import json
import math
import os
from pathlib import Path
import re
import sys
import tempfile

import numpy as np
import onnxruntime as ort
from tokenizers import Tokenizer


VERSION = 3
DIMENSION = 384
BATCH_SIZE = 32
READER_FLOOR = -0.75
MODEL_FILES = {
    "model.onnx": "b941bf19f1f1283680f449fa6a7336bb5600bdcd5f84d10ddc5cd72218a0fd21",
    "tokenizer.json": "be50c3628f2bf5bb5e3a7f17b1f74611b2561a3a27eeab05e5aa30f411572037",
    "reranker.onnx": "c80a8b34256ea453093d612e3ac48d3d965a0c0a48c7906709af8b8e28461bf9",
    "reranker-tokenizer.json": "d241a60d5e8f04cc1b2b3e9ef7a4921b27bf526d9f6050ab90f9267a1f9e5c66",
    "reader.onnx": "b573474247f705a1d0bd21997e690d8b7f8bc4379066fc11498bcdd08f735b6b",
    "reader-tokenizer.json": "2bb1a22cfbe25b8e5a232b7fc4d7fc5073923b45724a5f813b00811bb6620f66",
    "answerability.onnx": "7037afc2d93bfabb15637a98f0a183eab8c3064dfd2b62d286d6f38a149cefe1",
    "answerability-tokenizer.json": "727009a8214ddfa5af1deedf1006d4d06e8e51e54aa5f03566263d4e19bfcdce",
}


class Encoder:
    def __init__(self, directory, model_file="model.onnx", tokenizer_file="tokenizer.json"):
        model = read_verified(directory / model_file)
        tokenizer = read_verified(directory / tokenizer_file)
        self.tokenizer = Tokenizer.from_str(tokenizer.decode("utf-8"))
        self.tokenizer.enable_truncation(max_length=256)
        pad = "[PAD]" if self.tokenizer.token_to_id("[PAD]") is not None else "<pad>"
        self.tokenizer.enable_padding(pad_token=pad, pad_id=self.tokenizer.token_to_id(pad))
        options = ort.SessionOptions()
        options.intra_op_num_threads = min(os.cpu_count() or 1, 4)
        options.inter_op_num_threads = 1
        self.model = ort.InferenceSession(
            model, options, providers=["CPUExecutionProvider"]
        )
        self.inputs = {field.name for field in self.model.get_inputs()}

    def forward(self, texts):
        tokens = self.tokenizer.encode_batch(texts)
        inputs = {
            "input_ids": np.array([item.ids for item in tokens], dtype=np.int64),
            "attention_mask": np.array([item.attention_mask for item in tokens], dtype=np.int64),
            "token_type_ids": np.array([item.type_ids for item in tokens], dtype=np.int64),
        }
        values = self.model.run(None, {key: inputs[key] for key in self.inputs})
        return values, tokens, inputs["attention_mask"]

    def encode(self, texts):
        result = np.empty((len(texts), DIMENSION), dtype=np.float32)
        positions = {}
        for index, text in enumerate(texts):
            positions.setdefault(text, []).append(index)
        # Similar lengths share a batch so one long span does not pad every
        # short sentence. Repeated text still retains every source coordinate.
        unique = sorted(positions, key=len)
        for start in range(0, len(unique), BATCH_SIZE):
            batch = unique[start : start + BATCH_SIZE]
            values, _, attention_mask = self.forward(batch)
            hidden = values[0]
            mask = attention_mask.astype(np.float32)[..., None]
            pooled = np.sum(hidden * mask, axis=1) / np.sum(mask, axis=1)
            norms = np.linalg.norm(pooled, axis=1, keepdims=True)
            if not np.isfinite(norms).all() or (norms <= 0).any():
                raise ValueError("encoder produced invalid vectors")
            for text, vector in zip(batch, pooled / norms):
                result[positions[text]] = vector
        return result


def read_verified(path):
    data = path.read_bytes()
    if hashlib.sha256(data).hexdigest() != MODEL_FILES[path.name]:
        raise ValueError(f"unexpected encoder file digest for {path.name}")
    return data


def build(path, model_path, request):
    rows = request["rows"]
    if model_path is None:
        with np.load(path, allow_pickle=False) as archive:
            if int(archive["version"]) != VERSION:
                raise ValueError("semantic index format changed. Run dcr semantic-index with the model directory")
            if str(archive["snapshot"]) == request["snapshot"]:
                return len(rows)
            model_path = Path(str(archive["model_path"]))
    # Check the query-time weights before publishing the index.
    for name in MODEL_FILES.keys() - {"model.onnx", "tokenizer.json"}:
        read_verified(model_path / name)
    model = Encoder(model_path)
    keys = np.array([
        hashlib.sha256(json.dumps(row, ensure_ascii=False, separators=(",", ":")).encode()).hexdigest()
        for row in rows
    ], dtype="S64")
    previous = reusable_vectors(path)
    vectors = np.empty((len(rows), DIMENSION), dtype=np.float32)
    new = []
    for i, key in enumerate(keys):
        vector = previous.get(key)
        if vector is None:
            new.append(i)
        else:
            vectors[i] = vector
    if new:
        vectors[new] = model.encode([rows[i][3] for i in new])
    # The previous index stays available while the new snapshot is embedded.
    file = tempfile.NamedTemporaryFile(dir=path.parent, prefix=f".{path.name}.", delete=False)
    temporary = Path(file.name)
    try:
        with file:
            np.savez(
                file,
                version=np.int64(VERSION),
                snapshot=request["snapshot"],
                model_path=str(model_path),
                ids=np.array([row[0] for row in rows], dtype=np.str_),
                ranges=np.array([row[1:3] for row in rows], dtype=np.int64).reshape(-1, 2),
                keys=keys,
                vectors=vectors,
            )
        temporary.replace(path)
    finally:
        temporary.unlink(missing_ok=True)
    return len(rows)


def reusable_vectors(path):
    try:
        with np.load(path, allow_pickle=False) as archive:
            if int(archive["version"]) != VERSION:
                return {}
            keys, vectors = archive["keys"], archive["vectors"]
    except (OSError, ValueError, KeyError):
        return {}
    if (
        keys.ndim != 1
        or keys.dtype != np.dtype("S64")
        or vectors.dtype != np.float32
        or vectors.shape != (len(keys), DIMENSION)
    ):
        return {}
    return {key: vector for key, vector in zip(keys, vectors) if abs(np.linalg.norm(vector) - 1.0) <= 0.01}


def search(path, request):
    with np.load(path, allow_pickle=False) as archive:
        if int(archive["version"]) != VERSION or str(archive["snapshot"]) != request["snapshot"]:
            raise ValueError("semantic index is out of date. Run dcr semantic-index for this store")
        model_path = Path(str(archive["model_path"]))
        ids, ranges, vectors = archive["ids"], archive["ranges"], archive["vectors"]
    if (
        ids.shape != (request["count"],)
        or ranges.dtype != np.int64
        or ranges.shape != (len(ids), 2)
        or vectors.dtype != np.float32
        or vectors.shape != (len(ids), DIMENSION)
    ):
        raise ValueError("invalid semantic index. Run dcr semantic-index for this store")
    if len(ids) == 0:
        return []
    scores = vectors @ Encoder(model_path).encode([request["query"]])[0]
    if not np.isfinite(scores).all() or (np.abs(scores) > 1.001).any():
        raise ValueError("invalid similarity scores in semantic index")
    ranked = np.argsort(-scores, kind="stable")[: request["limit"]]
    return [
        [str(ids[i]), int(ranges[i, 0]), int(ranges[i, 1]), float(np.clip(scores[i], -1.0, 1.0))]
        for i in ranked
    ]


def rerank(path, request):
    with np.load(path, allow_pickle=False) as archive:
        if int(archive["version"]) != VERSION or str(archive["snapshot"]) != request["snapshot"]:
            raise ValueError("semantic index changed during retrieval. Retry context-read")
        model_path = Path(str(archive["model_path"]))
    candidates = request["candidates"]
    # Keep short passages together so a long candidate does not pad the
    # entire shortlist to its token length.
    candidates.sort(key=lambda row: len(row[4]))
    if request["lookup"]:
        question = request["query"].strip().rstrip(".?")
        return judge_answers(model_path, question + "?", candidates)

    logits = pair_scores(model_path, "reranker", request["query"], candidates)
    result = []
    for (key, start, end, cosine, _), logit in zip(candidates, logits):
        if cosine < 0.22 or (logit < -10.4 and cosine < 0.55):
            continue
        support = max(0.0, min(1.0, (float(logit) + 10.4) / 15.0))
        score = cosine + 0.35 * support * (1.0 - cosine)
        result.append([key, start, end, score, None, None])
    return sort_hits(result)


def pair_scores(model_path, name, query, candidates):
    model = Encoder(model_path, f"{name}.onnx", f"{name}-tokenizer.json")
    scores = []
    for offset in range(0, len(candidates), 8):
        pairs = [(query, row[4]) for row in candidates[offset : offset + 8]]
        values, _, _ = model.forward(pairs)
        logits = values[0].reshape(-1)
        if logits.shape != (len(pairs),) or not np.isfinite(logits).all():
            raise ValueError(f"{name} produced invalid scores")
        scores.extend(logits)
    return scores


def candidate_answer(token, starts, ends):
    # The first token is the reader's no-answer alternative. Consider only
    # spans inside the passage and keep start and end in the same word window.
    valid = [i for i, seq in enumerate(token.sequence_ids)
             if seq == 1 and token.offsets[i][0] < token.offsets[i][1]]
    if not valid:
        return None
    best = (-np.inf, 0, 0)
    for number, i in enumerate(valid):
        j = max(valid[number : number + 24], key=lambda k: ends[k])
        score = starts[i] + ends[j]
        if score > best[0]:
            best = (score, token.offsets[i][0], token.offsets[j][1])
    score, start, end = best
    return float(score - starts[0] - ends[0]), start, end


def judge_answers(model_path, query, candidates):
    model = Encoder(model_path, "reader.onnx", "reader-tokenizer.json")
    judged = []
    for offset in range(0, len(candidates), 8):
        batch = candidates[offset : offset + 8]
        values, tokens, _ = model.forward([(query, row[4]) for row in batch])
        starts, ends = values
        if starts.shape != ends.shape or len(starts) != len(batch) or not all(np.isfinite(x).all() for x in values):
            raise ValueError("extractive reader produced invalid scores")
        for row, token, first, last in zip(batch, tokens, starts, ends):
            answer = candidate_answer(token, first, last)
            if answer is not None:
                margin, start, end = answer
                if row[3] < 0.16 and margin < 1.0:
                    continue
                text = row[4]
                if text[start:end].strip(" .,:;!?\t\n\r") and answer_matches_question(query, text[start:end], margin):
                    judged.append((row, margin, start, end))
    # A second question/passage model breaks ties when the reader is
    # uncertain about every match. An accepted span takes precedence over
    # loosely related passages the second model might accept on their own.
    if judged and max(margin for _, margin, _, _ in judged) < READER_FLOOR:
        question = query.strip()
        attributed = "According to the passage, " + question[:1].lower() + question[1:]
        entailment = pair_scores(model_path, "answerability", attributed, [row for row, _, _, _ in judged])
    else:
        entailment = [None] * len(judged)
    fallback_floor = max(0.18, 0.52 * max(row[3] for row in candidates))
    hits = []
    for (row, margin, first, last), support in zip(judged, entailment):
        key, start, end, cosine, text = row
        if margin < READER_FLOOR:
            confirmed = cosine >= fallback_floor and support is not None and (
                support >= 0.0 or (support >= -3.0 and cosine >= 0.28)
            )
            if not confirmed or margin < -7.0:
                continue
            quality = 0.26 + 0.18 * sigmoid(support + 2)
        else:
            quality = 0.48 + 0.42 * sigmoid(margin / 2) + 0.06 * max(cosine, 0.0)
        # Byte ranges refer back to the exact UTF-8 passage Rust supplied.
        focus = [len(text[:first].encode()), len(text[:last].encode())] if start < end else [None, None]
        hits.append([key, start, end, quality, *focus])
    return sort_hits(hits)


def answer_matches_question(query, answer, margin):
    # Quantity questions need an amount. A location or a payment method can
    # be strongly related to a price question while supplying no price.
    question = query.lower()
    asks_quantity = re.search(r"\b(how many|what (?:is the )?(?:price|quantity|amount))\b", question)
    asks_price = "how much" in question and re.search(r"\b(cost|costs|charge|pay|price)\b", question)
    if any(char.isnumeric() for char in answer):
        return True
    words = set(re.findall(r"[a-z]+", answer.lower()))
    if margin < 0.0 and question.lstrip().startswith("when "):
        return bool(words & {
            "before", "after", "during", "until", "upon", "when", "once", "since", "by",
            "arrives", "today", "tomorrow", "yesterday", "tonight", "soon", "later", "now",
            "am", "pm", "noon", "midnight", "morning", "afternoon", "evening", "sunset", "dawn",
            "daily", "weekly", "next", "immediately", "hour", "hours", "day", "days",
            "week", "weeks", "month", "months", "year", "years", "spring", "summer", "autumn", "winter",
            "monday", "tuesday", "wednesday", "thursday", "friday", "saturday", "sunday",
            "january", "february", "march", "april", "may", "june", "july", "august",
            "september", "october", "november", "december",
        })
    if not asks_quantity and not asks_price:
        return True
    return bool(words & {
        "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten",
        "eleven", "twelve", "thirteen", "fourteen", "fifteen", "sixteen", "seventeen", "eighteen", "nineteen",
        "twenty", "thirty", "forty", "fifty", "sixty", "seventy", "eighty", "ninety",
        "hundred", "thousand", "million", "billion", "half", "quarter", "none", "free", "unlimited",
        "all", "several", "few", "dozens",
    }) or "no charge" in answer.lower()


def sigmoid(x):
    return 1.0 / (1.0 + math.exp(max(-60.0, min(60.0, -float(x)))))


def sort_hits(hits):
    return sorted(hits, key=lambda row: (-row[3], row[:3]))


if __name__ == "__main__":
    try:
        command, index, *args = sys.argv[1:]
        request = json.load(sys.stdin)
        if command == "build":
            result = build(Path(index), Path(args[0]), request)
        elif command == "refresh":
            result = build(Path(index), None, request)
        elif command == "search":
            result = search(Path(index), request)
        elif command == "rerank":
            result = rerank(Path(index), request)
        else:
            raise ValueError(f"unknown semantic index operation {command!r}")
        print(json.dumps(result, allow_nan=False))
    except (OSError, ValueError, RuntimeError, KeyError) as error:
        sys.exit(str(error))
