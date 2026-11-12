import hashlib, io, json, os, pathlib, platform, subprocess, tarfile, tempfile, time, urllib.request

output=pathlib.Path(os.environ["BENCHMARK_OUTPUT"])
output.mkdir(parents=True,exist_ok=True)
temporary=pathlib.Path(tempfile.mkdtemp(prefix="sc-benchmark-"))
target=temporary/"target"
hardware={
    "platform":platform.platform(),
    "cpus":os.cpu_count(),
    "cpuinfo":pathlib.Path("/proc/cpuinfo").read_text(),
    "meminfo":pathlib.Path("/proc/meminfo").read_text(),
    "source":subprocess.check_output(["git","rev-parse","HEAD"],text=True).strip(),
    "rustc":subprocess.check_output(["rustc","-Vv"],text=True),
    "cargo":subprocess.check_output(["cargo","-V"],text=True),
}
(output/"hardware.json").write_text(json.dumps(hardware,indent=2))
subprocess.run(["cargo","fetch","--locked"],check=True)
records=[]
for version,digest in [
    ("0.18.0","45f1447fbe231e3037bde351ef70677dd212216c8d62ae7ca409fecc4d6acc89"),
    ("0.19.0","aa73c7270c997894038c0d8f0d297865f57569e1687883322bc431713d4bb9dd"),
]:
    url=f"https://github.com/mozilla/sccache/releases/download/v{version}/sccache-v{version}-x86_64-unknown-linux-musl.tar.gz"
    with urllib.request.urlopen(url,timeout=60) as r:data=r.read()
    assert hashlib.sha256(data).hexdigest()==digest
    with tarfile.open(fileobj=io.BytesIO(data)) as archive:
        archive.extractall(temporary,filter="data")
    binary=temporary/f"sccache-v{version}-x86_64-unknown-linux-musl"/"sccache"
    socket_dir=pathlib.Path(tempfile.mkdtemp(prefix="sc-socket-"))
    env=dict(os.environ,RUSTC_WRAPPER=str(binary),SCCACHE_DIR=str(temporary/f"cache-{version}"),
        SCCACHE_SERVER_UDS=str(socket_dir/"server.sock"),SCCACHE_IDLE_TIMEOUT="0",
        CARGO_TARGET_DIR=str(target),CARGO_INCREMENTAL="0",CARGO_BUILD_JOBS="4",
        CARGO_PROFILE_DEV_DEBUG="0")
    subprocess.run([str(binary),"--version"],env=env,check=True)
    subprocess.run([str(binary),"--help"],env=env,check=True)
    subprocess.run([str(binary),"--start-server"],env=env,check=True)
    try:
        for state in ["cold","warm"]:
            subprocess.run(["cargo","clean","--target-dir",str(target)],env=env,check=True)
            subprocess.run([str(binary),"--zero-stats"],env=env,check=True)
            started=time.monotonic()
            with (output/f"{version}-{state}-build.log").open("w") as log:
                result=subprocess.run(["cargo","build","--offline","--locked","-p","okilum-shell","-p","okilum-sync-supervisor"],env=env,stdout=log,stderr=subprocess.STDOUT)
            elapsed=time.monotonic()-started
            stats=subprocess.check_output([str(binary),"--show-stats","--stats-format=json"],env=env,text=True)
            (output/f"{version}-{state}-stats.json").write_text(stats)
            record={"sccache":version,"state":state,"elapsed_seconds":elapsed,"returncode":result.returncode,"stats":json.loads(stats)}
            records.append(record)
            (output/"timings.json").write_text(json.dumps(records,indent=2)+"\n")
            print(json.dumps({k:v for k,v in record.items() if k!="stats"}),flush=True)
            if result.returncode:
                print((output/f"{version}-{state}-build.log").read_text()[-20000:],flush=True)
                raise SystemExit(result.returncode)
    finally:
        subprocess.run([str(binary),"--stop-server"],env=env)
