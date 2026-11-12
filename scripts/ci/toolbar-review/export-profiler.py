"""Read existing Time Profiler data; never launch the application again."""
import json
from pathlib import Path
import re
import subprocess
import sys
import xml.etree.ElementTree as ET

trace=Path(sys.argv[1]);out=Path(sys.argv[2]);root=ET.parse(out/'toc.xml').getroot()
results=[]
for run in root.findall('.//run'):
    for table in run.findall('./data/table'):
        schema=table.attrib['schema']
        item={'run':run.attrib['number'],'schema':schema,'attributes':table.attrib,'exported':False}
        if re.search(r'thread|sched|time-profile|context-switch|cpu-profile',schema,re.I):
            path=out/('table-'+str(len(results))+'.xml')
            xpath='/trace-toc/run[@number="'+run.attrib['number']+'"]/data/table[@schema="'+schema+'"]'
            p=subprocess.run(['xcrun','xctrace','export','--input',str(trace),'--output',str(path),'--xpath',xpath],capture_output=True,text=True)
            item.update({'exported':p.returncode==0,'file':path.name,'exit':p.returncode,'stdout':p.stdout,'stderr':p.stderr})
        results.append(item)
(out/'tables.json').write_text(json.dumps(results,indent=2))
print(json.dumps(results,indent=2))
