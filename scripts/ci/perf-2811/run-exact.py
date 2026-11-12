"""Run the existing cc4 binary exactly once; preserve strict original assertions."""
import ast
import hashlib
import json
import os
from pathlib import Path
import re
import struct
import subprocess
import sys
import time

SOURCE='cc4e0ac6e933e2f32bc062b4e470d4c4c8a3ebba'
BINARY_SHA='3b5dd87cd00b2b2dc0889d49ae083410e26797919d2f50b4c067889b418617d3'
FIXTURE_SHA='3111144a60615a0a1870164cfc22dae16d60a5a7c0813c7e3e74d294ad47af88'
ROOT=Path(__file__).resolve().parents[3]

def main():
    assert os.name=='nt'
    import _winapi
    import msvcrt
    binary=Path(sys.argv[1]).resolve();out=Path(sys.argv[2]).resolve()
    out.mkdir(parents=True,exist_ok=False)
    raw=binary.read_bytes();assert hashlib.sha256(raw).hexdigest()==BINARY_SHA
    opt=struct.unpack_from('<I',raw,0x3c)[0]+24;assert struct.unpack_from('<H',raw,opt)[0]==0x20b
    reserve,commit=struct.unpack_from('<QQ',raw,opt+72)
    assert (reserve,commit)==(1048576,4096)
    info=subprocess.check_output([str(binary),'--build-info'],text=True)
    assert SOURCE in info and 'gpui-test-support: no' in info
    (out/'build-info.txt').write_text(info,encoding='utf-8')
    script=subprocess.check_output(['git','show',SOURCE+':scripts/ci/code-toolbar-native-app.py'],cwd=ROOT,text=True,encoding='utf-8')
    tree=ast.parse(script);function=next(x for x in tree.body if isinstance(x,ast.FunctionDef) and x.name=='fixture')
    scope={'json':json};exec(compile(ast.Module(body=[function],type_ignores=[]),'<pinned fixture>','exec'),scope)
    text=scope['fixture']();fixture=out/'fixture';(fixture/'vault').mkdir(parents=True)
    path=fixture/'vault/api.json';path.write_text(text,encoding='utf-8')
    assert hashlib.sha256(path.read_bytes()).hexdigest()==FIXTURE_SHA
    env=dict(os.environ,OKILUM_CODE_TOOLBAR_NATIVE_FIXTURE=str(fixture))
    for key in ['OKILUM_CODE_TOOLBAR_NATIVE_SMOKE','OKILUM_DIAG_CENSUS','OKILUM_ATLAS_TRACE','OKILUM_ATLAS_UPLOAD_CONTROL','OKILUM_TREE_STYLE_CONTROL','OKILUM_TREE_LABEL_CONTROL','OKILUM_GLYPH_KEYS']:
        env.pop(key,None)
    # The native UI remains the primary CreateProcess thread. Default flags and
    # the original PE stack are retained; no priority/affinity/stack overrides.
    with (out/'native.log').open('wb',buffering=0) as log,open(os.devnull,'rb') as input_file:
        for stream in [log,input_file]:os.set_handle_inheritable(msvcrt.get_osfhandle(stream.fileno()),True)
        si=subprocess.STARTUPINFO();si.dwFlags|=_winapi.STARTF_USESTDHANDLES
        si.hStdInput=msvcrt.get_osfhandle(input_file.fileno());si.hStdOutput=msvcrt.get_osfhandle(log.fileno());si.hStdError=si.hStdOutput
        before=time.time_ns()
        process,thread,pid,tid=_winapi.CreateProcess(str(binary),subprocess.list2cmdline([str(binary)]),None,None,True,0,env,None,si)
        _winapi.CloseHandle(thread)
        wait=_winapi.WaitForSingleObject(process,180000)
        timeout=wait==_winapi.WAIT_TIMEOUT
        if timeout:
            _winapi.TerminateProcess(process,1);_winapi.WaitForSingleObject(process,10000)
        exit_code=_winapi.GetExitCodeProcess(process);_winapi.CloseHandle(process)
        after=time.time_ns()
    content=(out/'native.log').read_bytes();frames=[];offset=0
    pattern=re.compile(r'^CODE2810_FRAME marks=(true|false) sample=(\d+) wall_ms=([^ ]+) update_ms=([^ ]+) thread_cpu_ms=(.*?) draw_ms=([^ ]+) clear_ms=([^ ]+) text_work=(.*?) subtree_work=(.*?) present_ms=([^ ]+)$')
    for line in content.splitlines(keepends=True):
        decoded=line.decode('utf-8',errors='replace').rstrip('\r\n');match=pattern.match(decoded)
        if match:
            frames.append({'marks':match[1]=='true','sample':int(match[2]),'wall_ms':float(match[3]),'update_ms':float(match[4]),'thread_cpu':match[5],'draw_ms':float(match[6]),'clear_ms':float(match[7]),'text_work':json.loads(match[8]),'subtree_work':json.loads(match[9]),'present_ms':float(match[10]),'log_byte_range':[offset,offset+len(line)]})
        offset+=len(line)
    receipt_path=fixture/'toolbar-receipt.json';receipt=json.loads(receipt_path.read_text(encoding='utf-8')) if receipt_path.exists() else None
    if receipt:
        assert receipt['source_sha']==SOURCE and receipt['mounted']=='Reader' and receipt['real_native_platform']
        assert receipt['measurement']=='production-release' and len(receipt['frames'])==96
        assert receipt['bytes']==3500000 and receipt['paths_keys']==414 and receipt['initial_rasterizations']>0
        assert all(f['rows_built']>0 for f in receipt['frames'])
        for marked in [False,True]:assert len({f['scroll_y'] for f in receipt['frames'] if f['marks']==marked})==48
        (out/'receipt.json').write_text(json.dumps(receipt,indent=2),encoding='utf-8')
    assert hashlib.sha256(path.read_bytes()).hexdigest()==FIXTURE_SHA
    assert hashlib.sha256(binary.read_bytes()).hexdigest()==BINARY_SHA
    result={'attempts':1,'source':SOURCE,'artifact_run':43326565102,'binary_sha256':BINARY_SHA,'binary_bytes':len(raw),'fixture_sha256':FIXTURE_SHA,'pe_stack_reserve':reserve,'pe_stack_commit':commit,'creation_flags':0,'pid':pid,'primary_tid':tid,'exit_code':exit_code,'timed_out':timeout,'process_start_observer_unix_ns':before,'process_end_observer_unix_ns':after,'release_receipt_complete':receipt is not None,'strict_native_gate_pass':exit_code==0 and receipt is not None,'frames_recorded':len(frames),'wall_misses':sum(f['wall_ms']>16 for f in frames),'glyph_bitmaps':sum(f['text_work']['work']['glyph_bitmap']['calls'] for f in frames),'max_wall_ms':max((f['wall_ms'] for f in frames),default=None),'thresholds_unchanged':{'open_wall_ms':100,'frame_wall_ms':16},'backend':'gpui-pre-windows 0.3.3 with checksum-pinned DirectX staging atlas patch from exact cc4 source','frame_anchor':'No absolute frame stamps in this original binary. Native stdout byte ranges and kernel FileIO are recorded for bounded external-anchor analysis; no exact split is assumed.','diagnostic_success_meaning':'Capture completed, not a changed production verdict. Original strict assertions remain enabled.'}
    (out/'case.json').write_text(json.dumps(result,indent=2),encoding='utf-8');(out/'frames.json').write_text(json.dumps(frames,indent=2),encoding='utf-8')
    print(json.dumps(result,indent=2),flush=True)

if __name__=='__main__':main()
