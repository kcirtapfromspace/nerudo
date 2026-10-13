"""Transport and safety tests: all model replies are mocked; no inference is claimed."""

import io
import json
import pathlib
import unittest
import urllib.error
from unittest import mock

import llm_ollama as ollama


def tool(name, properties=None, required=None):
    return {"type": "function", "function": {"name": name, "parameters": {
        "type": "object", "properties": properties or {}, "required": required or [],
        "additionalProperties": False,
    }}}


def call(name, arguments=None):
    return {"function": {"name": name, "arguments": arguments if arguments is not None else {}}}


def reply(content="", calls=None):
    message = {"role": "assistant", "content": content}
    if calls is not None:
        message["tool_calls"] = calls
    return {"message": message, "done": True, "eval_count": 3,
            "prompt_eval_count": 10, "total_duration": 12345}


CATALOG = [
    tool("read_file", {"path": {"type": "string"}}, ["path"]),
    tool("git_diff"), tool("run_checks"),
    tool("git_start", {"branch": {"type": "string"}}, ["branch"]), tool("nerudo_sync"),
    tool("write_file", {"path": {"type": "string"}, "content": {"type": "string"}}, ["path", "content"]),
    tool("git_commit"), tool("nerudo_land", {"change_id": {"type": "string"}}),
    tool("nerudo_receipt", {"change_id": {"type": "string"}}, ["change_id"]),
]


class TransportTests(unittest.TestCase):
    def test_native_nonstreaming_request_and_bounded_response(self):
        transport_reply = reply("hello")
        stream = io.BytesIO(json.dumps(transport_reply).encode())
        with mock.patch.object(ollama.urllib.request, "urlopen", return_value=stream) as request:
            response = ollama._post_chat("http://127.0.0.1:11434/", {"stream": False}, 2.5)
        sent = request.call_args.args[0]
        self.assertEqual(sent.full_url, "http://127.0.0.1:11434/api/chat")
        self.assertEqual(sent.method, "POST")
        self.assertEqual(json.loads(sent.data), {"stream": False})
        self.assertEqual(request.call_args.kwargs["timeout"], 2.5)
        self.assertEqual(response, transport_reply)

    def test_rejects_oversized_invalid_incomplete_or_wrong_role_response(self):
        bad_responses = [b" " * (ollama.MAX_RESPONSE_BYTES + 1), b"invalid-json",
                         b"[]", b'{"message":{"role":"assistant"},"done":false}',
                         b'{"message":{"role":"tool"},"done":true}']
        for raw in bad_responses:
            with self.subTest(length=len(raw)):
                with mock.patch.object(ollama.urllib.request, "urlopen", return_value=io.BytesIO(raw)):
                    with self.assertRaises(ollama.TransportError):
                        ollama._post_chat("http://localhost:11434", {}, 1)

    def test_transport_errors_do_not_echo_server_body_or_url_credentials(self):
        error = urllib.error.HTTPError("http://localhost", 401, "secret-marker", {}, io.BytesIO(b"secret-marker"))
        with mock.patch.object(ollama.urllib.request, "urlopen", side_effect=error):
            with self.assertRaisesRegex(ollama.TransportError, "HTTP 401") as raised:
                ollama._post_chat("http://localhost:11434", {}, 1)
        self.assertNotIn("secret-marker", str(raised.exception))


READ_PRICE = call("read_file", {"path": "src/price.ts"})
READ_STYLE = call("read_file", {"path": "tests/price.test.mjs"})
WRITE_SOURCE = call("write_file", {"path": "src/receipt.ts", "content": "receipt implementation"})
WRITE_TEST = call("write_file", {"path": "tests/receipt.test.mjs", "content": "runtime tests"})
LAND = call("nerudo_land", {"change_id": "test-change"})


def planner():
    return [reply(calls=[READ_PRICE]), reply(calls=[call("complete_plan", {
        "summary": "Use the actual API; test receipt output.",
    })])]


def candidate():
    return [READ_PRICE, READ_STYLE, WRITE_SOURCE, WRITE_TEST, call("run_checks"), call("git_commit"), LAND]


def reviewer(approve=True):
    return [reply(calls=[call("git_diff"), READ_PRICE, call("run_checks")]),
            reply(calls=[call("complete_review", {"approve": approve, "summary": "Inspected diff and checks."})])]


def receipt(label="LANDED"):
    return {"isError": False, "content": [{"type": "text", "text": label + ": test-change rev 1 actual receipt"}]}


class WorkflowTests(unittest.TestCase):
    def run_responses(self, responses, *, callback=None, **options):
        events, executed, requests = [], [], []

        def execute(name, arguments):
            executed.append((name, arguments))
            if callback:
                return callback(name, arguments)
            return receipt() if name == "nerudo_land" else {"ok": True, "exit_code": 0}

        responses = iter(responses)

        def transport(url, payload, timeout):
            self.assertGreater(timeout, 0)
            self.assertFalse(payload["stream"])
            requests.append(json.loads(json.dumps(payload)))
            return next(responses, reply("Now I will continue."))

        with mock.patch.object(ollama, "_post_chat", side_effect=transport):
            result = ollama.run_ollama(pathlib.Path("/tmp/ollama-unit-workspace"), "Implement a price API.",
                                       CATALOG, execute, events.append, model=options.pop("model", "test-model"),
                                       max_turns=options.pop("max_turns", 4), **options)
        return result, events, executed, requests

    def test_records_actual_phases_and_preserves_native_call_messages(self):
        inspect = json.loads(json.dumps(READ_PRICE))
        inspect["id"] = "call-1"
        result, events, executed, requests = self.run_responses(
            [reply(calls=[inspect]), reply(calls=[call("complete_plan", {"summary": "Use inspected API."})]),
             reply(calls=candidate()), *reviewer(), reply("Actual receipt landed.")])
        self.assertTrue(result["success"])
        self.assertTrue(result["landed"])
        self.assertEqual(result["counters"]["inference_calls"], 6)
        self.assertEqual(result["counters"]["subagent_calls"], 2)
        self.assertEqual(result["counters"]["external_tool_calls"], 11)
        self.assertEqual(result["counters"]["eval_count"], 18)
        names = [name for name, _ in executed]
        self.assertLess(names.index("git_diff"), names.index("nerudo_land"))
        self.assertEqual(requests[1]["messages"][2]["tool_calls"], [inspect])
        self.assertEqual(requests[1]["messages"][3]["tool_name"], "read_file")
        self.assertEqual(requests[1]["messages"][3]["tool_call_id"], "call-1")
        self.assertEqual(requests[-1]["messages"][2]["tool_calls"][-1]["function"]["name"], "nerudo_land")
        self.assertEqual([e["role"] for e in events if e["type"] == "subagent_started"], ["planner", "reviewer"])
        for index in [0, 1, 3, 4]:
            allowed = {t["function"]["name"] for t in requests[index]["tools"]}
            self.assertTrue({"write_file", "nerudo_land", "delegate"}.isdisjoint(allowed))

    def test_intermediate_main_prose_continues_with_actual_state_until_landing(self):
        result, events, executed, requests = self.run_responses(
            [*planner(), reply("Now I will implement it."),
             reply(calls=[READ_PRICE, READ_STYLE, WRITE_SOURCE]), reply("Now I will add tests and commit."),
             reply(calls=[WRITE_TEST, call("run_checks"), call("git_commit"), LAND]),
             *reviewer(), reply("Landed.")], max_turns=5)
        self.assertTrue(result["success"])
        reminders = [e for e in events if e["type"] == "continuation_required" and e["actor"] == "main"]
        self.assertEqual(len(reminders), 2)
        self.assertIn('"written_files":["src/receipt.ts"]', reminders[1]["reason"])
        self.assertIn('"committed":false', reminders[1]["reason"])
        self.assertEqual(requests[3]["messages"][-1]["role"], "user")
        self.assertEqual(requests[3]["messages"][-2]["content"], "Now I will implement it.")

    def test_main_completion_claim_without_receipt_fails_boundedly(self):
        result, events, executed, requests = self.run_responses([*planner(), reply("LANDED: trust me.")], max_turns=2)
        self.assertFalse(result["success"])
        self.assertFalse(result["landed"])
        self.assertEqual(result["status"], "limit_reached")
        self.assertEqual(result["counters"]["main_inference_calls"], 2)
        self.assertEqual([name for name, _ in executed], ["read_file"])
        self.assertIsNone(result["review"])

    def test_planner_must_read_actual_source_before_finishing(self):
        result, events, executed, requests = self.run_responses(
            [reply("Invent a cents / 100 implementation."), *planner(),
             reply(calls=candidate()), *reviewer(), reply("Landed.")])
        self.assertTrue(result["success"])
        self.assertTrue(any(e["type"] == "continuation_required" and e["actor"] == "planner-1" for e in events))
        self.assertIn("read_file", requests[1]["messages"][-1]["content"])

    def test_reviewer_prose_requires_explicit_verdict(self):
        result, events, executed, requests = self.run_responses(
            [*planner(), reply(calls=candidate()), reply("Looks good. Now I will review."),
             *reviewer(), reply("Landed.")])
        self.assertTrue(result["success"])
        self.assertEqual(result["counters"]["reviewer_calls"], 1)
        self.assertTrue(any(e["type"] == "continuation_required" and e["actor"] == "reviewer-1" for e in events))
        self.assertIn("complete_review", requests[4]["messages"][-1]["content"])

    def test_source_read_test_write_and_no_gitignore_are_enforced(self):
        result, events, executed, requests = self.run_responses([
            *planner(), reply(calls=[WRITE_SOURCE, call("git_commit"), LAND,
                                     call("write_file", {"path": ".gitignore", "content": "src/receipt.ts"})]),
            reply(calls=candidate()), *reviewer(), reply("Landed.")])
        self.assertTrue(result["success"])
        self.assertEqual(result["counters"]["tool_errors"], 4)
        self.assertNotIn(("write_file", {"path": ".gitignore", "content": "src/receipt.ts"}), executed)
        errors = [e["result"] for e in events if e["type"] == "tool_rejected"]
        self.assertTrue(any("Read current src/price.ts" in result for result in errors))
        self.assertTrue(any("runtime tests" in result for result in errors))

    def test_subagents_cannot_write_or_delegate(self):
        result, events, executed, requests = self.run_responses([
            reply(calls=[call("write_file", {"path": "x", "content": "bad"}),
                         call("delegate", {"role": "planner", "task": "recurse"})]),
            *planner(), reply(calls=candidate()), *reviewer(), reply("Landed.")])
        self.assertTrue(result["success"])
        self.assertEqual(result["counters"]["tool_errors"], 2)
        rejected = [e for e in events if e["type"] == "tool_rejected"]
        self.assertEqual([e["name"] for e in rejected], ["write_file", "delegate"])

    def test_reviewer_must_inspect_diff_and_successful_checks(self):
        check_count = 0
        def execute(name, arguments):
            nonlocal check_count
            if name == "run_checks":
                check_count += 1
                return {"ok": check_count == 1, "exit_code": 0 if check_count == 1 else 2}
            return {"ok": True}

        result, events, executed, requests = self.run_responses([
            *planner(), reply(calls=candidate()),
            reply(calls=[call("complete_review", {"approve": True, "summary": "No inspection."})]),
            reply(calls=[call("git_diff"), call("run_checks")]),
            reply(calls=[call("complete_review", {"approve": True, "summary": "Ignore checks."})]),
            reply(calls=[call("complete_review", {"approve": False, "summary": "Checks failed."})]),
            reply("Cannot land."),
        ], callback=execute, max_turns=2)
        self.assertFalse(result["success"])
        self.assertNotIn("nerudo_land", [name for name, _ in executed])
        self.assertFalse(result["review"]["approve"])
        self.assertEqual(result["counters"]["tool_errors"], 4)

    def test_unknown_malformed_and_invalid_arguments_are_recoverable(self):
        result, events, executed, requests = self.run_responses([
            *planner(),
            reply(calls=[call("unknown"), {"function": []}, call("read_file", "{"),
                         call("read_file", {"path": 7}), call("read_file", {}),
                         call("read_file", '{"path":"valid.ts"}')]),
            reply(calls=candidate()), *reviewer(), reply("Landed.")])
        self.assertTrue(result["success"])
        self.assertIn(("read_file", {"path": "valid.ts"}), executed)
        self.assertEqual(result["counters"]["tool_errors"], 5)

    def test_callback_errors_and_oversized_outputs_remain_tool_results(self):
        def execute(name, arguments):
            if arguments.get("path") == "missing":
                raise FileNotFoundError("Missing input.")
            if arguments.get("path") == "long":
                return {"ok": True, "content": "x" * 100000}
            return receipt() if name == "nerudo_land" else {"ok": True, "exit_code": 0}

        result, events, executed, requests = self.run_responses([
            *planner(), reply(calls=[call("read_file", {"path": "missing"}), call("read_file", {"path": "long"})]),
            reply(calls=candidate()), *reviewer(), reply("Landed.")], callback=execute)
        self.assertTrue(result["success"])
        self.assertEqual(result["counters"]["tool_errors"], 1)
        self.assertEqual(result["counters"]["truncated_outputs"], 1)
        results = [e for e in requests[3]["messages"] if e["role"] == "tool"]
        self.assertEqual(json.loads(results[0]["content"])["error_type"], "FileNotFoundError")
        self.assertTrue(json.loads(results[1]["content"])["truncated"])
        self.assertLess(len(results[1]["content"]), 6500)

    def test_human_seal_stops_without_success_or_retry(self):
        def execute(name, arguments):
            return receipt("PARKED FOR A HUMAN") if name == "nerudo_land" else {"ok": True, "exit_code": 0}
        result, events, executed, requests = self.run_responses(
            [*planner(), reply(calls=candidate()), *reviewer(), reply("A human seal is required.")], callback=execute)
        self.assertFalse(result["success"])
        self.assertFalse(result["landed"])
        self.assertEqual(result["status"], "awaiting_approval")
        self.assertEqual(result["counters"]["main_inference_calls"], 1)

    def test_only_receipt_for_the_submitted_change_can_establish_landing(self):
        def execute(name, arguments):
            if name == "nerudo_land":
                return receipt("QUEUED")
            if name == "nerudo_receipt":
                return receipt()
            return {"ok": True, "exit_code": 0}

        result, events, executed, requests = self.run_responses(
            [*planner(), reply(calls=candidate()), *reviewer(),
             reply(calls=[call("nerudo_receipt", {"change_id": "another-change"})]),
             reply("Another change landed; done."),
             reply(calls=[call("nerudo_receipt", {"change_id": "test-change"})]),
             reply("Our submitted change landed.")], callback=execute, max_turns=5)
        self.assertTrue(result["success"])
        observations = [e for e in events if e["type"] == "landed_receipt_observed"]
        self.assertEqual(len(observations), 1)
        self.assertEqual(observations[0]["change_id"], "test-change")

    def test_truncated_output_and_deadline_never_claim_completion(self):
        limited = reply("unfinished")
        limited["done_reason"] = "length"
        result, events, executed, requests = self.run_responses([limited])
        self.assertFalse(result["success"])
        self.assertIn("output token limit reached", result["error"])
        events, executed = [], []
        with mock.patch.object(ollama.time, "monotonic", return_value=1000):
            workflow = ollama._Workflow(pathlib.Path("/tmp/unit"), "task", CATALOG,
                                        lambda *args: executed.append(args), events.append,
                                        "http://localhost:11434", "test", 1, 2)
        with mock.patch.object(ollama.time, "monotonic", return_value=1002):
            result = workflow.run()
        self.assertEqual(result["status"], "limit_reached")
        self.assertEqual(result["counters"]["inference_calls"], 0)
        self.assertEqual(executed, [])

    def test_delegation_has_fresh_conversation_and_total_budget(self):
        result, events, executed, requests = self.run_responses([
            *planner(), reply(calls=[call("delegate", {"role": "planner", "task": "Inspect callers independently."})]),
            *planner(), reply(calls=candidate()), *reviewer(), reply("Landed.")])
        self.assertTrue(result["success"])
        self.assertEqual(result["counters"]["planner_calls"], 2)
        self.assertEqual(len(requests[3]["messages"]), 2)
        self.assertEqual(requests[3]["messages"][1]["content"], "Inspect callers independently.")
        workflow = ollama._Workflow(pathlib.Path("/tmp/unit"), "task", CATALOG, lambda *args: {},
                                    lambda event: None, "http://localhost:11434", "test", 60, 2)
        workflow.counters["subagent_calls"] = ollama.MAX_SUBAGENTS
        with self.assertRaisesRegex(ollama.WorkflowLimit, "subagent budget"):
            workflow.delegate("planner", "task")

    def test_qwen3_uses_documented_thinking_control_and_smaller_limits(self):
        result, events, executed, requests = self.run_responses(
            [*planner(), reply(calls=candidate()), *reviewer(), reply("Landed.")], model="qwen3:4b")
        self.assertTrue(result["success"])
        self.assertTrue(all(r["think"] is False for r in requests))
        self.assertTrue(all(r["options"]["num_ctx"] == 8192 for r in requests))
        self.assertTrue(all(r["options"]["num_predict"] == 1024 for r in requests))

    def test_small_model_uses_4k_context_and_bounded_grounded_plan(self):
        result, events, executed, requests = self.run_responses(
            [reply(calls=[READ_PRICE]), reply(calls=[call("complete_plan", {"summary": "step " * 400})]), reply(calls=candidate()),
             *reviewer(), reply("Landed.")], model="qwen3:1.7b")
        self.assertTrue(result["success"])
        self.assertTrue(all(r["options"]["num_ctx"] == 4096 for r in requests))
        self.assertTrue(all(r["think"] is False for r in requests))
        self.assertEqual(len(result["planner"]["summary"].split()), 350)
        self.assertIn("full response recorded", result["planner"]["summary"])
        planner_tools = {t["function"]["name"] for t in requests[0]["tools"]}
        self.assertEqual(planner_tools, {"read_file", "complete_plan"})

    def test_grounded_planner_budget_failure_is_recorded_and_primary_attempts_tools(self):
        absolute_read = call("read_file", {"path": "/tmp/ollama-unit-workspace/src/price.ts"})
        result, events, executed, requests = self.run_responses(
            [reply(calls=[absolute_read]), *[reply("I will finish planning.") for _ in range(5)],
             reply(calls=candidate()), *reviewer(), reply("Actual landing succeeded.")])
        self.assertTrue(result["success"])
        self.assertTrue(result["planner"]["failed"])
        self.assertTrue(result["planner"]["source_inspected"])
        self.assertGreater(result["counters"]["main_inference_calls"], 0)
        self.assertIn("write_file", [name for name, _ in executed])
        self.assertTrue(any(e["type"] == "planner_failure_nonfatal" for e in events))
        self.assertIn("No completed plan is available", requests[6]["messages"][1]["content"])

    def test_planner_failure_without_source_inspection_cannot_be_faked(self):
        result, events, executed, requests = self.run_responses([reply("Planning done.") for _ in range(6)])
        self.assertFalse(result["success"])
        self.assertFalse(result["planner"]["source_inspected"])
        self.assertEqual(result["counters"]["main_inference_calls"], 0)
        self.assertEqual(executed, [])

    def test_complete_plan_requires_source_read_and_absolute_main_paths_normalize(self):
        absolute_candidate = [
            call("read_file", {"path": "/tmp/ollama-unit-workspace/src/price.ts"}),
            call("read_file", {"path": "/tmp/ollama-unit-workspace/tests/price.test.mjs"}),
            call("write_file", {"path": "/tmp/ollama-unit-workspace/src/receipt.ts", "content": "receipt"}),
            call("write_file", {"path": "/tmp/ollama-unit-workspace/tests/receipt.test.mjs", "content": "tests"}),
            call("run_checks"), call("git_commit"), LAND,
        ]
        result, events, executed, requests = self.run_responses(
            [reply(calls=[call("complete_plan", {"summary": "No source read."})]),
             *planner(), reply(calls=absolute_candidate), *reviewer(), reply("Landed.")])
        self.assertTrue(result["success"])
        self.assertEqual(result["counters"]["tool_errors"], 1)
        self.assertTrue(result["planner"]["source_inspected"])

    def test_planner_receives_acceptance_only_not_native_execution_workflow(self):
        prompt = "You are the provider.\nImplement task T-3: Format receipts.\nAcceptance criteria:\n" \
                 "- Use shared price API.\n- Add real tests.\n\nCall native workflow tools and recover conflicts."
        task = ollama._Workflow.planning_task(prompt)
        self.assertIn("Implement task T-3", task)
        self.assertIn("Add real tests", task)
        self.assertNotIn("native workflow", task)
        self.assertNotIn("You are the provider", task)

    def test_small_model_compacts_whole_tool_turns_with_actual_state(self):
        def execute(name, arguments):
            if name == "read_file" and arguments["path"].startswith("bulk-"):
                return {"path": arguments["path"], "content": "x" * 3000}
            return receipt() if name == "nerudo_land" else {"ok": True, "exit_code": 0}

        bulky_reads = [reply(calls=[call("read_file", {"path": f"bulk-{n}"})]) for n in range(6)]
        result, events, executed, requests = self.run_responses(
            [*planner(), *bulky_reads, reply(calls=candidate()), *reviewer(), reply("Landed.")],
            callback=execute, model="qwen3:1.7b", max_turns=8)
        self.assertTrue(result["success"])
        self.assertTrue(any(e["type"] == "context_compacted" and e["actor"] == "main" for e in events))
        compacted = [request for request in requests if any(
            "Earlier complete tool turns" in item.get("content", "") for item in request["messages"])]
        self.assertTrue(compacted)
        for request in compacted:
            messages = request["messages"]
            self.assertEqual(messages[0]["role"], "system")
            self.assertTrue(messages[1]["content"].startswith("Implement a price API."))
            for index, message in enumerate(messages):
                if message["role"] == "tool":
                    self.assertTrue(index > 0 and messages[index - 1]["role"] in {"assistant", "tool"})

    def test_branch_and_sync_invalidate_reads_checks_and_review_and_change_exposure(self):
        before_land = candidate()[:-1]
        result, events, executed, requests = self.run_responses([
            *planner(), reply(calls=[call("git_start", {"branch": "first"})]),
            reply(calls=before_land),
            reply(calls=[call("delegate", {"role": "reviewer", "task": "Inspect current candidate."})]),
            *reviewer(), reply(calls=[call("nerudo_sync")]),
            reply(calls=[call("git_start", {"branch": "retry"}), WRITE_SOURCE]),
            reply(calls=candidate()), *reviewer(), reply("Landed.")], max_turns=7)
        self.assertTrue(result["success"])
        names = lambda request: {tool["function"]["name"] for tool in request["tools"]}
        self.assertNotIn("git_start", names(requests[3]))
        self.assertIn("nerudo_land", names(requests[4]))
        self.assertIn("git_start", names(requests[8]))
        self.assertTrue({"write_file", "run_checks", "git_commit", "nerudo_land"}.isdisjoint(names(requests[8])))
        self.assertEqual(len([name for name, _ in executed if name == "write_file"]), 4)
        invalidations = [event for event in events if event["type"] == "candidate_state_invalidated"]
        self.assertEqual([event["branch_started"] for event in invalidations], [True, False, True])
        self.assertEqual(result["counters"]["reviewer_calls"], 2)
        self.assertTrue(result["review_current"])
        self.assertEqual(result["counters"]["tool_errors"], 1)

    def test_test_write_requires_an_actual_read_of_existing_test_style(self):
        result, events, executed, requests = self.run_responses([
            *planner(), reply(calls=[READ_PRICE, WRITE_TEST]), reply(calls=candidate()),
            *reviewer(), reply("Landed.")])
        self.assertTrue(result["success"])
        self.assertEqual(result["counters"]["tool_errors"], 1)
        rejected = next(event for event in events if event["type"] == "tool_rejected")
        self.assertIn("tests/price.test.mjs", rejected["result"])
        self.assertEqual(len([name for name, _ in executed if name == "write_file"]), 2)

    def test_repeated_identical_returned_failures_are_counted_and_require_recovery(self):
        failed_branch_calls = 0

        def execute(name, arguments):
            nonlocal failed_branch_calls
            if name == "git_start" and arguments["branch"] == "existing":
                failed_branch_calls += 1
                return {"ok": False, "error": "A branch named existing already exists"}
            return receipt() if name == "nerudo_land" else {"ok": True, "exit_code": 0}

        same = reply(calls=[call("git_start", {"branch": "existing"})])
        result, events, executed, requests = self.run_responses([
            *planner(), same, same, same,
            reply(calls=[call("git_start", {"branch": "new-unique"})]),
            reply(calls=candidate()), *reviewer(), reply("Landed.")], callback=execute, max_turns=6)
        self.assertTrue(result["success"])
        self.assertEqual(failed_branch_calls, 2)
        self.assertEqual(result["counters"]["tool_errors"], 3)
        guidance = [event["guidance"] for event in events if event["type"] == "repeated_tool_failure"]
        self.assertTrue(guidance)
        self.assertTrue(all("NEW unique branch name" in item for item in guidance))
        self.assertTrue(any("Actual last result" in item for item in guidance))

    def test_qwen25_3b_uses_the_same_4k_request_memory_safeguards(self):
        result, events, executed, requests = self.run_responses(
            [*planner(), reply(calls=candidate()), *reviewer(), reply("Landed.")], model="qwen2.5:3b")
        self.assertTrue(result["success"])
        self.assertTrue(all(request["options"]["num_ctx"] == 4096 for request in requests))
        self.assertTrue(all(request["options"]["num_predict"] == 1024 for request in requests))
        self.assertTrue(all("think" not in request for request in requests))

    def test_transport_failure_and_invalid_inputs_never_claim_success(self):
        events = []
        with mock.patch.object(ollama, "_post_chat", side_effect=ollama.TransportError("Connection failed")):
            result = ollama.run_ollama(pathlib.Path("/tmp/unit"), "task", CATALOG, lambda *args: {}, events.append, model="test")
        self.assertFalse(result["success"])
        self.assertEqual(result["status"], "failed")
        self.assertEqual(result["counters"]["inference_calls"], 1)
        arguments = {"workspace": pathlib.Path("/tmp/unit"), "prompt": "task", "tool_catalog": CATALOG,
                     "execute_tool": lambda *args: {}, "emit_event": lambda event: None, "model": "test"}
        cases = [{"url": "http://user:secret@localhost:11434"}, {"timeout": 0}, {"timeout": True},
                 {"max_turns": 0}, {"model": ""}, {"prompt": ""}, {"tool_catalog": [None]},
                 {"tool_catalog": [CATALOG[0], CATALOG[0]]}, {"tool_catalog": [ollama.DELEGATE_TOOL]}]
        with mock.patch.object(ollama, "_post_chat") as transport:
            for changes in cases:
                with self.subTest(changes=changes), self.assertRaises(ValueError):
                    ollama.run_ollama(**{**arguments, **changes})
            transport.assert_not_called()


if __name__ == "__main__":
    unittest.main()
