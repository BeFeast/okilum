#!/usr/bin/env python3
"""Loopback-only fake T3 discovery. No outgoing network, real credentials or provider."""
import argparse
import asyncio
import json
import pathlib
import tempfile
from aiohttp import web, ClientSession

TOKEN = 'synthetic-only-319'  # Public fixture literal; never a live credential.

class FakeT3:
    def __init__(self, directory, environment, project):
        self.directory = pathlib.Path(directory)
        self.environment, self.project = environment, project
        self.calls = []
        self.forbidden = 0
        self.tickets = 0

    def report(self):
        return {'schema': 'tessera-319-fake-t3/v1', 'synthetic_only': True,
                'environment': self.environment, 'project': self.project,
                'rpc_calls': self.calls, 'ticket_requests': self.tickets,
                'forbidden_provider_or_unknown_calls': self.forbidden,
                'real_provider_work_possible': False}

    def persist(self):
        self.directory.mkdir(parents=True, exist_ok=True)
        temporary = self.directory / 'observations.tmp'
        temporary.write_text(json.dumps(self.report(), indent=2) + '\n')
        temporary.replace(self.directory / 'observations.json')

    async def ticket(self, request):
        if request.headers.get('Authorization') != 'Bearer ' + TOKEN:
            return web.json_response({'error': 'synthetic_token_required'}, status=401)
        self.tickets += 1
        self.persist()
        return web.json_response({'ticket': 'synthetic-ticket-319'})

    async def socket(self, request):
        if request.query.get('wsTicket') != 'synthetic-ticket-319':
            return web.Response(status=401)
        socket = web.WebSocketResponse()
        await socket.prepare(request)
        async for message in socket:
            if message.type != web.WSMsgType.TEXT:
                continue
            value = json.loads(message.data)
            if value.get('_tag') in ('Ack', 'Interrupt', 'Pong'):
                continue
            tag = value.get('tag', '<missing>')
            self.calls.append(tag)  # Never retain user content or auth.
            key = value.get('id')
            if value.get('_tag') == 'Request' and tag == 'server.getConfig':
                reply = {'_tag': 'Exit', 'requestId': key, 'exit': {'_tag': 'Success', 'value': {
                    'environment': {'environmentId': self.environment},
                    'providers': [{'instanceId': 'synthetic-provider', 'models': [
                        {'slug': 'synthetic-model', 'name': 'Synthetic model'}]}]}}}
            elif value.get('_tag') == 'Request' and tag == 'orchestration.subscribeShell':
                reply = {'_tag': 'Chunk', 'requestId': key, 'values': [
                    {'kind': 'snapshot', 'snapshot': {'projects': [
                        {'id': self.project, 'title': 'Synthetic candidate project'}]}},
                    {'kind': 'synchronized'}]}
            else:
                self.forbidden += 1
                reply = {'_tag': 'Exit', 'requestId': key, 'exit': {
                    '_tag': 'Failure', 'cause': 'synthetic_fixture_refuses_provider_work'}}
            self.persist()
            await socket.send_json(reply)
        return socket

    async def unexpected(self, request):
        self.forbidden += 1
        self.calls.append('HTTP ' + request.method + ' ' + request.path)
        self.persist()
        return web.json_response({'error': 'synthetic_fixture_refuses_unknown_endpoint'}, status=403)

    def app(self):
        app = web.Application()
        app.router.add_post('/api/auth/websocket-ticket', self.ticket)
        app.router.add_get('/ws', self.socket)
        app.router.add_route('*', '/{tail:.*}', self.unexpected)
        return app

async def listen(fixture, port):
    runner = web.AppRunner(fixture.app())
    await runner.setup()
    site = web.TCPSite(runner, '127.0.0.1', port)
    await site.start()
    return runner, site._server.sockets[0].getsockname()[1]

async def self_test():
    with tempfile.TemporaryDirectory(prefix='tessera-319-fake-t3-') as path:
        fixture = FakeT3(path, 'synthetic-new-environment', 'synthetic-project')
        runner, port = await listen(fixture, 0)
        try:
            async with ClientSession() as client:
                base = f'http://127.0.0.1:{port}'
                response = await client.post(base + '/api/auth/websocket-ticket', headers={'Authorization': 'Bearer ' + TOKEN})
                assert (await response.json())['ticket'] == 'synthetic-ticket-319'
                async with client.ws_connect(base + '/ws?wsTicket=synthetic-ticket-319') as socket:
                    for tag in ['server.getConfig', 'orchestration.subscribeShell']:
                        await socket.send_json({'_tag': 'Request', 'id': '1', 'tag': tag, 'payload': {}})
                        assert (await socket.receive_json())['requestId'] == '1'
                    assert fixture.forbidden == 0
                    await socket.send_json({'_tag': 'Request', 'id': '2', 'tag': 'orchestration.dispatchCommand', 'payload': {'type': 'thread.turn.start'}})
                    assert (await socket.receive_json())['exit']['_tag'] == 'Failure'
                    assert fixture.forbidden == 1  # Positive control detects actual WS work request.
        finally:
            await runner.cleanup()
        print('PASS: actual HTTP/WS discovery and forbidden provider-call positive control; isolated fixture only')

async def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--directory')
    parser.add_argument('--port', type=int, default=0)
    parser.add_argument('--environment', default='synthetic-new-environment')
    parser.add_argument('--project', default='synthetic-project')
    parser.add_argument('--self-test', action='store_true')
    args = parser.parse_args()
    if args.self_test:
        return await self_test()
    if not args.directory or not pathlib.Path(args.directory).is_absolute():
        parser.error('--directory requires a fresh absolute synthetic directory')
    directory = pathlib.Path(args.directory)
    directory.mkdir(parents=True, exist_ok=False)
    fixture = FakeT3(directory, args.environment, args.project)
    fixture.persist()
    runner, port = await listen(fixture, args.port)
    ready = {'base_url': f'http://127.0.0.1:{port}', 'environment_id': args.environment,
             'project_id': args.project, 'token_env': 'env:TESSERA_319_FAKE_TOKEN',
             'model_instance_id': 'synthetic-provider', 'model': 'synthetic-model',
             'runtime_mode': 'approval-required', 'interaction_mode': 'default'}
    (directory / 'candidate.json').write_text(json.dumps(ready, indent=2) + '\n')
    print(json.dumps(ready), flush=True)
    try:
        await asyncio.Event().wait()
    finally:
        await runner.cleanup()

if __name__ == '__main__':
    asyncio.run(main())
