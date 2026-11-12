#!/usr/bin/env python3
"""Fixed diagnostic census. Green means complete data, never a budget waiver."""
import argparse
import ast
import hashlib
import json
import os
from pathlib import Path
import platform
import statistics
import subprocess
import time

ROOT=Path(__file__).resolve().parents[3]
BASES={'main':'774cef4abf61d39147069918f8ba8a1000008187',
       'toolbar':'863bfdd3a50f699087f5d81d154dd59d1cdcd202'}
FIXTURE_SHA='3111144a60615a0a1870164cfc22dae16d60a5a7c0813c7e3e74d294ad47af88'


def fixture():
    raw=subprocess.check_output(['git','show',BASES['toolbar']+':scripts/ci/code-toolbar-native-app.py'],cwd=ROOT,text=True)
    tree=ast.parse(raw)
    function=next(node for node in tree.body if isinstance(node,ast.FunctionDef) and node.name=='fixture')
    namespace={'json':json}
    exec(compile(ast.Module(body=[function],type_ignores=[]),'<pinned fixture>','exec'),namespace)
    text=namespace['fixture']()
    assert hashlib.sha256(text.encode()).hexdigest()==FIXTURE_SHA
    return text


def phase(frames):
    return {'frames':len(frames),'misses':sum(f['cpu_draw_ms']>16 for f in frames),
            'max_wall_ms':max(f['cpu_draw_ms'] for f in frames),
            'median_wall_ms':statistics.median(f['cpu_draw_ms'] for f in frames),
            'new_bitmaps':sum(f['text_work']['work']['glyph_bitmap']['calls'] for f in frames)}


def run_case(inputs,kind,out,text,census=True,profiler=False,template="Time Profiler"):
    out.mkdir(parents=True,exist_ok=False)
    manifest=json.loads((inputs/kind/'capsule-manifest.json').read_text())
    assert manifest['base_source']==BASES[kind]
    binary=inputs/kind/('okilum.exe' if os.name=='nt' else 'okilum')
    binary=binary.resolve()
    if os.name!='nt':binary.chmod(0o755)
    info=subprocess.check_output([str(binary),'--build-info'],text=True)
    (out/'build-info.txt').write_text(info)
    assert manifest['controller_source'] in info and 'gpui-test-support: no' in info
    root=out/'fixture';(root/'vault').mkdir(parents=True)
    path=root/'vault/api.json';path.write_text(text,encoding='utf-8')
    assert hashlib.sha256(path.read_bytes()).hexdigest()==FIXTURE_SHA
    env=dict(os.environ,OKILUM_CODE_TOOLBAR_NATIVE_FIXTURE=str(root))
    for name in ['OKILUM_CODE_TOOLBAR_NATIVE_SMOKE','OKILUM_ATLAS_TRACE','OKILUM_ATLAS_UPLOAD_CONTROL',
                 'OKILUM_TREE_STYLE_CONTROL','OKILUM_TREE_LABEL_CONTROL','OKILUM_GLYPH_KEYS']:
        env.pop(name,None)
    if census:env['OKILUM_DIAG_CENSUS']='1'
    else:env.pop('OKILUM_DIAG_CENSUS',None)
    args=[str(binary)]
    if profiler:
        args=['xcrun','xctrace','record','--template',template,'--output',str(out/'reader.trace'),
              '--launch','--',str(binary)]
    started=time.time_ns()
    with (out/'code-toolbar-native-app.log').open('wb') as log:
        process=subprocess.run(args,env=env,stdout=log,stderr=subprocess.STDOUT,timeout=180)
    result={'kind':kind,'base_source':manifest['base_source'],'controller_source':manifest['controller_source'],
            'binary_sha256':hashlib.sha256(binary.read_bytes()).hexdigest(),'fixture_sha256':FIXTURE_SHA,
            'exit':process.returncode,'census':census,'profiler':profiler,'start_unix_ns':started,
            'end_unix_ns':time.time_ns(),'valid':False}
    receipt_path=root/'toolbar-receipt.json'
    if receipt_path.exists():
        receipt=json.loads(receipt_path.read_text())
        assert receipt['source_sha']==manifest['controller_source']
        assert receipt['real_native_platform'] and receipt['mounted']=='Reader'
        assert receipt['measurement']=='production-release'
        assert receipt['bytes']==3500000 and receipt['paths_keys']==414
        assert receipt['initial_rasterizations']>0
        frames=receipt['frames'];assert len(frames)==96
        for marked in [False,True]:
            group=[f for f in frames if f['marks']==marked]
            assert len(group)==48 and len({f['scroll_y'] for f in group})==48
            assert all(f['rows_built']>0 and f['present_ms']>=0 for f in group)
        assert hashlib.sha256(path.read_bytes()).hexdigest()==FIXTURE_SHA
        result.update({'valid':True,'open_ms':receipt['open_ms'],
                       'strict_wall_pass':receipt['open_ms']<=100 and all(f['cpu_draw_ms']<=16 for f in frames),
                       'backend':'merged-staging-upload',
                       'max_thread_cpu_ms':max((f.get('thread_cpu_ms') or 0) for f in frames),
                       'phases':{str(marked):phase([f for f in frames if f['marks']==marked]) for marked in [False,True]}})
        (out/'receipt.json').write_text(json.dumps(receipt,indent=2))
    (out/'case.json').write_text(json.dumps(result,indent=2))
    print(json.dumps(result),flush=True)
    return result


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--inputs',type=Path,required=True)
    parser.add_argument('--out',type=Path,required=True)
    parser.add_argument('--mode',choices=['pairs','etw','mac','mac-profiler'],required=True)
    parser.add_argument('--profiler-template',default='Time Profiler')
    args=parser.parse_args();args.inputs=args.inputs.resolve();args.out=args.out.resolve()
    args.out.mkdir(parents=True,exist_ok=True)
    text=fixture()
    host={'platform':platform.platform(),'processor':platform.processor(),'cpus':os.cpu_count(),
          'runner_name':os.environ.get('RUNNER_NAME'),'controller':subprocess.check_output(['git','rev-parse','HEAD'],cwd=ROOT,text=True).strip(),
          'thresholds_unchanged':{'open_wall_ms':100,'frame_wall_ms':16},
          'meaning':'diagnostic census captures every frame; no failed frame is relabeled as a gate pass'}
    (args.out/'host.json').write_text(json.dumps(host,indent=2))
    if args.mode=='pairs':
        plan=[(i,k) for i in range(5) for k in (['main','toolbar'] if i%2==0 else ['toolbar','main'])]
    elif args.mode=='etw':plan=[(0,'main'),(0,'toolbar')]
    elif args.mode=='mac':plan=[(i,'toolbar') for i in range(5)]
    else:plan=[(0,'toolbar')]
    (args.out/'plan.json').write_text(json.dumps(plan,indent=2))
    results=[]
    for i,kind in plan:
        results.append(run_case(args.inputs,kind,args.out/f'{i}-{kind}',text,profiler=args.mode=='mac-profiler',template=args.profiler_template))
        (args.out/'results.json').write_text(json.dumps(results,indent=2))
    assert all(r['valid'] for r in results), 'Incomplete cases cannot establish relative miss rates'
    if args.mode in ['pairs','mac']:
        summary={}
        for kind in sorted({k for _,k in plan}):
            rows=[r for r in results if r['kind']==kind]
            summary[kind]={'runs':len(rows),'runs_with_wall_miss':sum(not r['strict_wall_pass'] for r in rows),
                           'frames':{},'open_max_ms':max(r['open_ms'] for r in rows)}
            for marked in [False,True]:
                groups=[r['phases'][str(marked)] for r in rows]
                summary[kind]['frames'][str(marked)]={'total':sum(g['frames'] for g in groups),
                    'misses':sum(g['misses'] for g in groups),'max_wall_ms':max(g['max_wall_ms'] for g in groups),
                    'new_bitmaps':sum(g['new_bitmaps'] for g in groups)}
        (args.out/'summary.json').write_text(json.dumps(summary,indent=2));print(json.dumps(summary,indent=2))


if __name__=='__main__':main()
