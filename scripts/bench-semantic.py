#!/usr/bin/env python3
"""Measure compact retrieval with a pinned encoder and a lexical control."""

import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time


# The sources use different wording from the questions. Other passages in the
# same store act as distractors for every query.
CASES = [
    ("Where did the baby nap?", "The infant slept inside a crib.", "crib"),
    ("What medication did the doctor recommend?", "The physician prescribed antibiotics.", "antibiotics"),
    ("Where did the cat snooze?", "The feline rested atop the sofa.", "sofa"),
    ("How much does it cost to rent a bike?", "Bicycles may be borrowed for $8 per hour.", "$8"),
    ("When does the swimming facility shut?", "The pool closes at 20:00.", "20:00"),
    ("How are faulty items sent back?", "Defective merchandise can be returned using prepaid postage.", "prepaid postage"),
    ("Which vehicle takes people from the airport to town?", "A shuttle connects the terminal with downtown.", "shuttle"),
    ("How long can staff keep borrowed equipment?", "Personnel must return loaned gear within fourteen days.", "fourteen days"),
    ("How do I change the password after forgetting it?", "Reset a lost passphrase using the recovery email.", "recovery email"),
    ("What happens when the internet stops working?", "An offline cache keeps locally saved reports accessible during connectivity outages.", "offline cache"),
    ("What does the mechanic put in an engine for lubrication?", "Motor oil prevents rubbing components from overheating.", "Motor oil"),
    ("Who should approve outgoing payments?", "The treasurer authorizes disbursements above $500.", "treasurer"),
    ("Which animals may fly in the cabin?", "Travelers are permitted to bring small pets onboard.", "small pets"),
    ("How can I get money back on a cancelled trip?", "Passenger refunds require an unused ticket.", "unused ticket"),
    ("What will happen if the shipment arrives too late?", "Delayed deliveries trigger a ten-percent credit.", "ten-percent credit"),
    ("Where can kids play outside?", "Children gather beside the swings in Greenway Park.", "Greenway Park"),
]

UNRELATED = [
    "How many moons orbit Jupiter?",
    "Which volcano erupted in Iceland?",
    "Describe Renaissance portraiture.",
    "Who won the chess championship?",
    "How is maple syrup made?",
    "What causes a rainbow?",
]

DECOYS = [
    "The baby screening kit ships on Tuesday.",
    "The aircraft cabin has blue LED lighting.",
    "The fastest bike in the race carried a green flag.",
    "The mechanic teaches an evening course in carpentry.",
    "The doctor collects vintage postcards.",
    "Town planning meetings are held overseas.",
    "The parcel code is ZZ-99.",
    "Correction: the parcel code is ZX-17.",
]

CHALLENGE = [
    ("Where do empty jars belong?", "Household glass should go into the blue bin.", "blue bin"),
    ("Who can sign off on a refund?", "Reimbursements require the finance manager's consent.", "finance manager"),
    ("When can I pick up the repaired laptop?", "Serviced notebooks will be ready Friday afternoon.", "Friday afternoon"),
    ("How do guests reach the building after dark?", "Visitors use the west entrance overnight.", "west entrance"),
    ("How much room is available for luggage?", "The baggage compartment holds eighty liters.", "eighty liters"),
    ("How must workers evacuate amid electrical outages?", "During a blackout, employees should use the emergency stairwell.", "emergency stairwell"),
    ("Where did she leave the borrowed novel?", "Mara returned the library book by dropping it into the outdoor slot.", "outdoor slot"),
    ("Where can I get my shoes fixed?", "A cobbler works from the shop on Rowan Street.", "Rowan Street"),
]

CHALLENGE_DECOYS = [
    "The building east entrance closes at sunset.",
    "The laptop repair desk accepts checks.",
    "The luggage tag goes near the handle.",
    "Jars for the kitchen can be purchased at the market.",
    "A spare novel was left on the bench.",
]

RELATED_WITHOUT_ANSWERS = [
    "How much does it cost to repair a laptop?",
    "When will the toy jars arrive?",
    "What color was the novel?",
    "What is the bike race winner called?",
    "Which public bus stops at the airport on Sundays?",
    "At what price are the kitchen jars sold?",
]


def run(binary, store, *args, query=None, check=True, evidence_bytes=None):
    env = os.environ.copy()
    env.pop("DCR_EVIDENCE_BYTES", None)
    if evidence_bytes is not None:
        env["DCR_EVIDENCE_BYTES"] = str(evidence_bytes)
    result = subprocess.run(
        [str(binary), "--store", str(store), *map(str, args)],
        input=query, capture_output=True, text=True, env=env,
    )
    if check and result.returncode:
        raise RuntimeError(f"{args[0]} failed for {query!r}: {result.stderr.strip()}")
    return result


def read(binary, store, budget, query, lexical=False, evidence_bytes=None):
    flags = ["--lexical-only"] if lexical else []
    result = run(
        binary, store, "context-read", "--budget", budget, *flags,
        query=query, evidence_bytes=evidence_bytes,
    )
    return json.loads(result.stdout)


def compare(binary, store, budget, cases):
    top = [0, 0]
    any_hit = [0, 0]
    for query, _, answer in cases:
        ranks = []
        for i, lexical in enumerate((True, False)):
            hits = read(binary, store, budget, query, lexical)
            assert sum((len(hit["text"].encode()) + 3) // 4 for hit in hits) <= budget
            rank = next(
                (n for n, hit in enumerate(hits, start=1) if answer.lower() in hit["text"].lower()),
                None,
            )
            top[i] += rank == 1
            any_hit[i] += rank is not None
            ranks.append(rank)
        assert ranks[0] is None, f"lexical-only fixture leaked the target for {query!r}"
        print(f"  {query:<59} {ranks[1] or 'miss'}")
    print(f"Top-1 lexical {top[0]}/{len(cases)}, hybrid {top[1]}/{len(cases)}")
    print(f"Found in budget lexical {any_hit[0]}/{len(cases)}, hybrid {any_hit[1]}/{len(cases)}")
    return top[1], any_hit[1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dcr", required=True, type=Path)
    parser.add_argument("--model", required=True, type=Path)
    parser.add_argument("--budget", type=int, default=24)
    args = parser.parse_args()
    binary = args.dcr.resolve()
    with tempfile.TemporaryDirectory(prefix="dcr-semantic-bench-") as directory:
        root = Path(directory)
        store = root / "memory.json"
        sources = root / "sources.txt"
        sources.write_text("\n\n".join([source for _, source, _ in CASES] + DECOYS), encoding="utf-8")
        run(binary, store, "ingest", sources)
        before = store.read_bytes()
        start = time.monotonic()
        print(run(binary, store, "semantic-index", "--semantic-model", args.model).stdout.strip())
        print(f"Index build: {time.monotonic() - start:.2f} seconds")
        basic = compare(binary, store, args.budget, CASES)
        unrelated = [query for query in UNRELATED if read(binary, store, args.budget, query)]
        print(f"Unrelated queries with evidence: {len(unrelated)}/{len(UNRELATED)}")
        assert not unrelated, unrelated
        corrected = read(binary, store, 12, "What is the parcel code?")
        assert corrected and "ZX-17" in corrected[0]["text"], corrected
        assert all("ZZ-99" not in hit["text"] for hit in corrected), corrected
        assert store.read_bytes() == before

        index = store.with_name(f"{store.name}.semantic.npz")
        older = index.read_bytes()
        additional = root / "additional.txt"
        additional.write_text("The auditorium seats two hundred spectators.", encoding="utf-8")
        run(binary, store, "ingest", additional)
        assert index.read_bytes() != older
        fresh = read(binary, store, args.budget, "How large is the venue?")
        assert fresh and "two hundred" in fresh[0]["text"], fresh
        index.write_bytes(older)
        stale = run(binary, store, "context-read", query="How large is the venue?", check=False)
        assert stale.returncode and "semantic index is out of date" in stale.stderr, stale.stderr

        print("Challenge set with close lexical distractors")
        store = root / "challenge"
        doc = root / "challenge.txt"
        doc.write_text("\n\n".join([source for _, source, _ in CHALLENGE] + CHALLENGE_DECOYS), encoding="utf-8")
        run(binary, store, "ingest", doc)
        run(binary, store, "semantic-index", "--semantic-model", args.model)
        challenge = compare(binary, store, args.budget, CHALLENGE)
        negatives = [
            query for query in RELATED_WITHOUT_ANSWERS
            if read(binary, store, args.budget, query)
        ]
        print(f"Related unanswered queries with evidence: {len(negatives)}/{len(RELATED_WITHOUT_ANSWERS)}")
        assert not negatives, negatives
        if args.budget == 24:
            assert basic == (len(CASES), len(CASES)), basic
            assert challenge == (len(CHALLENGE), len(CHALLENGE)), challenge
            bare_question = read(binary, store, 24, CHALLENGE[-1][0].rstrip("?"))
            assert bare_question and CHALLENGE[-1][2] in bare_question[0]["text"], bare_question

        print("Duplicate passages cannot crowd out the answer")
        store = root / "repeated-passage"
        doc = root / "repeated-passage.txt"
        query, source, answer = CHALLENGE[0]
        doc.write_text("\n\n".join([source] + [CHALLENGE_DECOYS[3]] * 75), encoding="utf-8")
        run(binary, store, "ingest", doc)
        run(binary, store, "semantic-index", "--semantic-model", args.model)
        hits = read(binary, store, 24, query)
        assert hits and answer in hits[0]["text"], hits

        print("Verbatim answer at the end of a long sentence")
        store = root / "long-passage"
        doc = root / "long-passage.txt"
        source = (
            "The map labels the neighboring blocks and their borders; " * 80
            + "the cobbler repairs footwear on Rówan Street."
        )
        doc.write_text(source, encoding="utf-8")
        run(binary, store, "ingest", doc)
        run(binary, store, "semantic-index", "--semantic-model", args.model)
        limit = 25
        hits = read(binary, store, 24, CHALLENGE[-1][0], evidence_bytes=limit)
        text = [hit["text"] for hit in hits]
        assert text and "Rówan Street" in text[0] and text[0] in source, hits
        assert sum(len(json.dumps(item, ensure_ascii=False).encode()) + 1 for item in text) <= limit
        print(f"  {text[0]!r} fits in {limit} encoded bytes")


if __name__ == "__main__":
    main()
