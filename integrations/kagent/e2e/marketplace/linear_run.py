#!/usr/bin/env python3
"""Linear + GitHub slice on native Python/Go kagent; fixture providers only.

Build all four appa-acceptance-*:linear images from this checkout first.
Usage: python3 linear_run.py /tmp/new-linear-acceptance-directory
"""
import json
from pathlib import Path
import sys
import time
import uuid
from run import Acceptance, REPO, http, require


class LinearAcceptance(Acceptance):
    image_tag = "linear"
    read_tool = "get_issue"

    def source_config(self):
        text = (REPO / 'examples/linear-battery/approved-writes.toml').read_text()
        text = '\n'.join(line for line in text.splitlines() if not line.startswith('include ='))
        # Ordinary native host entry gates; the two batteries supply MCP policy.
        for name in ('host/kagent/ask_user', 'host/kagent-gate/code_execution',
                     'host/kagent-gate/memory_persist', 'host/kagent-gate/a2a_delegate'):
            text += f'\n[[policy.tool]]\nname = "{name}"\ndelta = {{}}\n'
        return text.encode()

    def install_batteries(self, appa, deployed, server):
        super().install_batteries(appa, deployed, server)
        return json.loads(self.command([appa, 'battery', 'install', 'linear', '--config', deployed,
                                       '--server', server, '--json']).splitlines()[-1])

    def resources(self, prepared, mirror, release, endpoint):
        resources = super().resources(prepared, mirror, release, endpoint)
        for resource in resources:
            if resource['kind'] == 'Agent':
                resource['spec']['declarative']['tools'][0]['mcpServer']['toolNames'] += ['get_issue', 'save_comment']
        return resources

    def read_call(self):
        return {'tool': 'get_issue', 'args': {'id': 'ENG-1'}}

    def approval_scenarios(self, agent_name, fixture_url):
        # Retain the native provider-tool confirmation regression, then exercise
        # APPA's dynamic Linear trust + attention review through native resume.
        super().approval_scenarios(agent_name, fixture_url)
        agent_url = self.forward(agent_name, 8080)

        def send(message):
            result = http(agent_url, {'jsonrpc': '2.0', 'id': uuid.uuid4().hex,
                                     'method': 'message/send', 'params': {'message': message}})
            deadline = time.monotonic() + 180
            while result.get('result', {}).get('status', {}).get('state') in ('submitted', 'working') and time.monotonic() < deadline:
                time.sleep(1)
                result = http(agent_url, {'jsonrpc': '2.0', 'id': uuid.uuid4().hex, 'method': 'tasks/get',
                                         'params': {'id': result['result']['id']}})
            return result

        for decision in ('reject', 'approve'):
            http(fixture_url + '/state', {})
            write = {'tool': 'save_comment', 'args': {'issueId': 'ENG-1', 'body': 'reviewed fixture text'}}
            script = [write, {'remedy': 'linear-operator'}, write, {'text': 'done'}]
            pending = send({'role': 'user', 'kind': 'message', 'messageId': uuid.uuid4().hex,
                            'parts': [{'kind': 'text', 'text': json.dumps({'appa_script': script})}]})
            require(pending.get('result', {}).get('status', {}).get('state') == 'input-required',
                    f'Linear review did not suspend: {pending}')
            require(not http(fixture_url + '/state')['invocations'], 'Linear write ran before review')
            task = pending['result']
            result = send({'role': 'user', 'kind': 'message', 'messageId': uuid.uuid4().hex,
                           'taskId': task['id'], 'contextId': task['contextId'],
                           'parts': [{'kind': 'data', 'data': {'decision_type': decision}}]})
            state = http(fixture_url + '/state')
            (self.work / f'{agent_name}-linear-review-{decision}.json').write_text(
                json.dumps({'pending': pending, 'task': result, 'fixture': state}, indent=2))
            require(result.get('result', {}).get('status', {}).get('state') == 'completed',
                    f'Linear review did not resume: {result}')
            require([r['index'] for r in state['requests']] == list(range(len(script))), 'incomplete Linear review script')
            require(state['invocations'] == ([{'tool': 'save_comment', 'args': write['args']}] if decision == 'approve' else []),
                    'wrong actual Linear execution after review')


if __name__ == '__main__':
    if len(sys.argv) != 2:
        raise SystemExit('usage: linear_run.py NEW_OUTPUT_DIRECTORY')
    acceptance = LinearAcceptance(Path(sys.argv[1]))
    try:
        acceptance.run()
    finally:
        acceptance.close()
