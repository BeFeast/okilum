"""Hosted-only isolated GET fixture. Rejects unknown hosts; never forwards."""
import hashlib,http.server,json,pathlib,ssl,threading
P=pathlib.Path(__file__).resolve().parent
ctx=ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
ctx.load_cert_chain(P/'leaf.pem',P/'leaf.key')
payload=b'isolated-updater-fixture'
class Handler(http.server.BaseHTTPRequestHandler):
 protocol_version='HTTP/1.1'
 def log_message(self,*args):pass
 def reply(self,status,body,kind='application/json'):
  self.send_response(status);self.send_header('Content-Type',kind);self.send_header('Content-Length',str(len(body)));self.send_header('Connection','close');self.end_headers();self.wfile.write(body);self.close_connection=True
 def do_CONNECT(self):
  if self.path.lower()!='updater.okilum.test:443':self.reply(403,b'{}');return
  self.send_response(200,'Connection established');self.end_headers();self.close_connection=True
  try:Handler(ctx.wrap_socket(self.connection,server_side=True),self.client_address,self.server)
  except (OSError,ssl.SSLError):pass
 def do_GET(self):
  with (P/'requests.jsonl').open('a') as f:f.write(json.dumps({'path':self.path,'host':self.headers.get('Host'),'user_agent':self.headers.get('User-Agent')})+'\n')
  if self.path=='/proxy.pac':self.reply(200,b'function FindProxyForURL(url,host){if(host=="updater.okilum.test")return "PROXY 127.0.0.1:18178";return "DIRECT";}','application/x-ns-proxy-autoconfig')
  elif self.path.split('?')[0]=='/updates/releases.stable.json':self.reply(200,json.dumps({'Assets':[{'PackageId':'Okilum.NetworkRegression','Version':'0.1.2','Type':'Full','FileName':'fixture.nupkg','Size':len(payload),'SHA256':hashlib.sha256(payload).hexdigest()}]}).encode())
  elif self.path=='/updates/fixture.nupkg':self.reply(200,payload,'application/octet-stream')
  else:self.reply(404,b'{}')
class Server(http.server.ThreadingHTTPServer):daemon_threads=True
proxy=Server(('127.0.0.1',18178),Handler)
direct=Server(('127.0.0.1',18179),Handler);direct.socket=ctx.wrap_socket(direct.socket,server_side=True)
threading.Thread(target=direct.serve_forever,daemon=True).start()
(P/'ready.json').write_text(json.dumps({'forwarding':False}))
proxy.serve_forever()
