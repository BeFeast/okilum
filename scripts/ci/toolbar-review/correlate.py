#!/usr/bin/env python3
"""Correlate precise native frame brackets with Windows kernel CSwitch data."""
import calendar
import datetime
import json
from pathlib import Path
import re
import sys
import xml.etree.ElementTree as ET


def stamp(text):
    second=datetime.datetime.strptime(text[:19],'%Y-%m-%dT%H:%M:%S')
    part=re.search(r'\.(\d+)',text)
    return 116444736000000000+calendar.timegm(second.timetuple())*10000000+(int(part[1][:7].ljust(7,'0')) if part else 0)


def number(value):
    if value is None:return None
    value=value.strip()
    try:return int(value,0)
    except ValueError:return int(value)


def correlate(root):
    cases=[];tids=set()
    for file in sorted(root.glob('*-*/receipt.json')):
        receipt=json.loads(file.read_text());frames=receipt['frames']
        ids={f['subtree_work']['native_memory_and_cycles']['tid'] for f in frames}
        assert len(ids)==1;tid=ids.pop();tids.add(tid)
        cases.append((file.parent,tid,frames))
    assert len(cases)==10, 'five complete main/toolbar pairs required'
    events={tid:[] for tid in tids};ready={tid:[] for tid in tids};lost=[]
    context=ET.iterparse(root/'reader.xml',events=['start','end'])
    _,document=next(context);count=0
    for action,event in context:
        if action!='end' or event.tag.rsplit('}',1)[-1]!='Event':continue
        data={e.attrib.get('Name'):e.text or '' for e in event.findall('.//{*}Data')}
        if 'EventsLost' in data:lost.append(number(data['EventsLost']))
        opcode=event.findtext('./{*}RenderingInfo/{*}Opcode')
        if opcode=='ReadyThread':
            tid=number(data.get('TThreadId'))
            if tid in tids:
                ready[tid].append(stamp(event.find('./{*}System/{*}TimeCreated').attrib['SystemTime']))
        if opcode=='CSwitch':
            old=number(data.get('OldThreadId'));new=number(data.get('NewThreadId'))
            if old in tids or new in tids:
                time=event.find('./{*}System/{*}TimeCreated').attrib['SystemTime']
                item=(stamp(time),old,new,number(data.get('OldThreadState')),number(data.get('OldThreadWaitReason')))
                for tid in tids:
                    if tid in [old,new]:events[tid].append(item)
        event.clear();count+=1
        if count%4096==0:document.clear()
    assert lost and all(value==0 for value in lost), ('ETW lost events',lost)
    results=[]
    for directory,tid,frames in cases:
        switches=sorted(events[tid]);assert len(switches)>4, 'UI thread scheduling observer did not fire'
        assert ready[tid], 'ReadyThread observer did not fire'
        bounds=[f['subtree_work']['native_memory_and_cycles']['wall_bounds_filetime_100ns'] for f in frames]
        assert switches[0][0]<bounds[0][0] and switches[-1][0]>bounds[-1][1], 'trace must cover the case'
        # Match only the lifetime containing this case: a later process may reuse TID.
        gaps=[];out=None
        for ticks,old,new,state,reason in switches:
            if old==tid:out=(ticks,state,reason)
            if new==tid and out:
                assert ticks>=out[0]
                a,state,reason=out
                if state==1:
                    gaps.append((a,ticks,1,reason))
                else:
                    transitions=[r for r in ready[tid] if a<=r<=ticks]
                    if transitions:
                        r=min(transitions)
                        gaps.extend([(a,r,state,reason),(r,ticks,1,reason)])
                    else:
                        # No transition is not proof that the entire gap stayed Waiting.
                        gaps.append((a,ticks,99,reason))
                out=None
        details=[]
        for frame in frames:
            left,right=frame['subtree_work']['native_memory_and_cycles']['wall_bounds_filetime_100ns']
            assert right>left
            bracket=(right-left)/10000;wall=frame['cpu_draw_ms'];margin=bracket-wall
            states={};overlaps=[]
            for a,b,state,reason in gaps:
                overlap=max(0,min(b,right)-max(a,left))/10000
                if overlap:
                    states[state]=states.get(state,0)+overlap
                    overlaps.append({'state':state,'reason':reason,'overlap_ms':overlap,'bounds_filetime_100ns':[a,b]})
            nonrunning=sum(states.values());running=bracket-nonrunning
            assert running>=-0.0001
            valid=abs(margin)<=0.1
            details.append({'marks':frame['marks'],'sample':frame['sample'],'wall_ms':wall,
                'wall_bracket_ms':bracket,'instrumentation_margin_ms':margin,'correlation_valid':valid,
                'ready_ms':states.get(1,0),'waiting_ms':states.get(5,0),
                'other_nonrunning_ms':sum(v for k,v in states.items() if k not in [1,5]),
                'running_bracket_ms':max(0,running),
                'running_frame_interval_ms':[max(0,running-max(0,margin)),max(0,running)],
                'switch_gaps':overlaps})
        result={'case':directory.name,'tid':tid,'events_lost':0,'frames':details,
                'misses':[f for f in details if f['wall_ms']>16],
                'note':'No time is subtracted from the unchanged production gate. An excessive bracket margin is NO_EVIDENCE.'}
        (directory/'correlation.json').write_text(json.dumps(result,indent=2));results.append(result)
    (root/'correlation.json').write_text(json.dumps(results,indent=2))
    for result in results:
        print(result['case'],'misses',len(result['misses']))
        for frame in result['misses']:
            print(json.dumps({k:frame[k] for k in ['marks','sample','wall_ms','running_frame_interval_ms','ready_ms','waiting_ms','other_nonrunning_ms','correlation_valid']}))


if __name__=='__main__':correlate(Path(sys.argv[1]))
