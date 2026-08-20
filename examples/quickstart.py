"""Smallest useful thing: ingest a transcript, ask a question, audit the answer.

    python examples/quickstart.py
"""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from dcr import DCR  # noqa: E402

runtime = DCR(budget=600)

runtime.ingest("Goal: restore checkout by 09:00 UTC.")
runtime.ingest('The error was "connection refused" when talking to the inventory host.')
runtime.ingest("The server ip is 10.0.4.12 and the port is 8080.")
runtime.ingest("The blocker is firewall rule 37, which drops checkout traffic.")
runtime.ingest("Decision: roll back to build 4471 because the blocker is firewall rule 37.")

# ... 200 turns of noise later ...
for i in range(200):
    runtime.ingest(f"Chatter {i}: dashboards refreshed, queue at {i} items, nothing to do.")

# ... and a correction nobody would find by scrolling.
runtime.ingest("Correction: actually the server ip is 10.0.9.7, we misread the dashboard.")

answer = runtime.ask("what is the server ip?")
print(answer.text)
print(f"({answer.tokens} tokens in the window, out of "
      f"{runtime.telemetry.history_tokens} tokens of history)\n")

# Every answer is walkable back to raw spans.
print(runtime.explain(answer.cited[0]))

# And you can see exactly what the model was shown, and why.
print("\n" + runtime.planner.explain_plan(answer.context))
