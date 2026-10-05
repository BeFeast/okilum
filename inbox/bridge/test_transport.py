import json
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from websockets.sync.server import serve

from t3_questions import Http, Inbox, T3, Unavailable

TOKEN = 'fixture-token-not-a-real-credential'


class TransportTests(unittest.TestCase):
    def test_http_auth_pagination_no_redirect_or_raw_error(self):
        seen = []
        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_):
                pass

            def do_GET(self):
                seen.append((self.path,self.headers.get('Authorization')))
                if self.path == '/redirect':
                    self.send_response(302); self.send_header('Location','/leak'); self.end_headers(); return
                if self.path == '/fail':
                    self.send_response(500); self.end_headers(); self.wfile.write(TOKEN.encode()); return
                if self.path.endswith('after=0&limit=100'):
                    body={'operations':[{'id':'first'}],'next_cursor':3,'has_more':True}
                elif self.path.endswith('after=3&limit=100'):
                    body={'operations':[{'id':'second'}],'next_cursor':8,'has_more':False}
                else:
                    body={'ok':True}
                self.send_response(200); self.end_headers(); self.wfile.write(json.dumps(body).encode())
        server=ThreadingHTTPServer(('127.0.0.1',0),Handler)
        thread=threading.Thread(target=server.serve_forever,daemon=True);thread.start()
        try:
            base=f'http://127.0.0.1:{server.server_port}'
            client=Http(base,TOKEN)
            self.assertEqual(client.request('GET','/ok'),{'ok':True})
            for route in ['/redirect','/fail']:
                with self.assertRaises(Unavailable) as error:client.request('GET',route)
                self.assertNotIn(TOKEN,str(error.exception))
            self.assertFalse(any(path=='/leak' for path,_ in seen))
            self.assertEqual(list(Inbox(base,TOKEN).operations()),[{'id':'first'},{'id':'second'}])
            self.assertTrue(all(token=='Bearer '+TOKEN for _,token in seen))
        finally:
            server.shutdown();server.server_close();thread.join()

    def test_native_rpc_ping_receipt_and_lost_ack_are_distinct(self):
        seen=[]
        def handler(socket):
            frame=json.loads(socket.recv(timeout=2));seen.append((socket.request,frame))
            socket.send('{"_tag":"Ping"}')
            self.assertEqual(json.loads(socket.recv(timeout=2)),{'_tag':'Pong'})
            if frame['payload']['requestId']=='lose-ack':
                return
            socket.send(json.dumps({'_tag':'Exit','requestId':frame['id'],
                                    'exit':{'_tag':'Success','value':{'sequence':20}}}))
        server=serve(handler,'127.0.0.1',0)
        thread=threading.Thread(target=server.serve_forever,daemon=True);thread.start()
        port=server.socket.getsockname()[1]
        client=T3(f'http://127.0.0.1:{port}',TOKEN)
        command={'type':'runtime-request.respond','commandId':'stable-op-id','threadId':'pilot',
                 'requestId':'native-question','answers':{'colour':'Blue'}}
        try:
            client.respond(command)
            request,frame=seen[0]
            self.assertEqual(request.path,'/ws?orchestrationProtocol=2')
            self.assertEqual(request.headers['Authorization'],'Bearer '+TOKEN)
            self.assertEqual(frame['tag'],'orchestration.dispatchCommand')
            self.assertEqual(frame['payload'],command)
            command=dict(command,requestId='lose-ack')
            with self.assertRaisesRegex(Unavailable,'source_outcome_uncertain'):client.respond(command)
            self.assertEqual(len(seen),2)  # transport never retries
            with self.assertRaises(Unavailable):client.respond(dict(command,type='thread.create'))
            self.assertEqual(len(seen),2)
        finally:
            server.shutdown();thread.join()


if __name__ == '__main__':
    unittest.main()
