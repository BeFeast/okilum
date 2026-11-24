# View snapshot HTML profile 1

Each JSON file is a sealed C2 view snapshot. Its sibling HTML file is the
complete expected output of `okilum-cored render --profile mobile
--view-snapshot <file.json>`. The seven files named after `view-golden` inputs
preserve those results, with snapshot version 1 and a fixed UTC seal time.
The synthetic `activity` pair also covers section errors, cached section
time in daylight saving time, typed links, diagnostics and truncated groups.
