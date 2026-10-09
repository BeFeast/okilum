"""Read a literal Compose invocation from existing operator scripts; never execute them."""
from dataclasses import dataclass
from pathlib import Path
import shlex
import re

ACTIONS = {'up', 'down', 'stop', 'start', 'restart', 'rm', 'exec', 'ps', 'config', 'build', 'pull', 'create', 'logs'}
OPTIONS = {'-f', '--file', '-p', '--project-name', '--project-directory', '--env-file', '--profile'}


@dataclass(frozen=True)
class Invocation:
    argv: tuple
    cwd: Path
    files: tuple
    env_files: tuple
    project: str | None
    scripts: tuple = ()

    @classmethod
    def from_scripts(cls, activate, rollback):
        first, second = cls.read(activate), cls.read(rollback)
        if first != second:
            raise ValueError('Activate and rollback Compose invocations differ; stop')
        return cls(first.argv, first.cwd, first.files, first.env_files, first.project,
                   (activate.resolve(), rollback.resolve()))

    @classmethod
    def read(cls, path):
        cwd, found = None, []
        for line in path.read_text().replace('\\\n', ' ').splitlines():
            tokens = shlex.split(line, comments=True)
            if not tokens:
                continue
            if tokens[0] in ('export', 'source', '.') or re.match(r'^[A-Za-z_][A-Za-z_0-9]*=', tokens[0]):
                raise ValueError('Script environment setup is unsupported; use a literal --env-file')
            if tokens[0] == 'cd':
                if len(tokens) != 2 or not Path(tokens[1]).is_absolute() or any(c in tokens[1] for c in '$`'):
                    raise ValueError('Script working directory must be literal and absolute')
                cwd = Path(tokens[1]).resolve()
            pairs = [i for i in range(len(tokens)-1) if tokens[i:i+2] == ['docker', 'compose']]
            for index in pairs:
                if cwd is None:
                    raise ValueError('Compose script must set an explicit working directory')
                leading = tokens[:index]
                if leading and leading[0] == 'if':
                    leading = leading[1:]
                if leading and leading[0] == '!':
                    leading = leading[1:]
                if leading not in ([], ['sudo'], ['sudo', '-n']):
                    raise ValueError('Unsupported Compose command prefix; stop')
                prefix = ['docker', 'compose']
                if index and tokens[index-1] == 'sudo':
                    prefix.insert(0, 'sudo')
                elif index > 1 and tokens[index-2:index] == ['sudo', '-n']:
                    prefix[0:0] = ['sudo', '-n']
                options, files, env_files, project = [], [], [], None
                project_dir = cwd
                cursor = index+2
                while cursor < len(tokens) and tokens[cursor] not in ACTIONS:
                    option = tokens[cursor]
                    if option not in OPTIONS or cursor+1 >= len(tokens):
                        raise ValueError('Unsupported or dynamic Compose option; stop')
                    value = tokens[cursor+1]
                    if any(c in value for c in '$`;&|<>') or value == '-':
                        raise ValueError('Compose options must be literal; stop')
                    options += [option, value]
                    absolute = (cwd/value).resolve()
                    if option in ('-f', '--file'):
                        files.append(absolute)
                    elif option == '--env-file':
                        env_files.append(absolute)
                    elif option in ('-p', '--project-name'):
                        project = value
                    elif option == '--project-directory':
                        project_dir = absolute
                    cursor += 2
                if cursor == len(tokens):
                    raise ValueError('Compose action not found')
                if not files:
                    # Accept only an unambiguous standard default pair. Runtime
                    # labels must match these paths before any mutation is allowed.
                    candidates = [project_dir/name for name in
                                  ('compose.yaml', 'compose.yml', 'docker-compose.yaml', 'docker-compose.yml')
                                  if (project_dir/name).is_file()]
                    if len(candidates) != 1:
                        raise ValueError('Implicit Compose files are ambiguous; use explicit -f in scripts')
                    files = candidates
                    base = 'docker-compose' if files[0].name.startswith('docker-compose') else 'compose'
                    overrides = [project_dir/(base+'.override'+ext) for ext in ('.yaml', '.yml')
                                 if (project_dir/(base+'.override'+ext)).is_file()]
                    if len(overrides) > 1:
                        raise ValueError('Implicit Compose overrides are ambiguous')
                    files += overrides
                found.append(cls(tuple(prefix+options), cwd, tuple(files), tuple(env_files), project))
        if not found or any(item != found[0] for item in found):
            raise ValueError('Script has no unique Compose invocation; stop')
        return found[0]

    def command(self, active, *args):
        prefix = list(self.argv)
        if active.exists():
            if not any(flag in prefix for flag in ('-f', '--file')):
                for path in self.files:
                    prefix += ['-f', str(path)]
            if active not in self.files:
                prefix += ['-f', str(active)]
        return prefix+list(args)

    def verify_labels(self, labels, active, resolved_project):
        expected_files = self.files + ((active,) if active.exists() and active not in self.files else ())
        actual_files = tuple(Path(p).resolve() for p in labels.get('com.docker.compose.project.config_files', '').split(',') if p)
        if actual_files != expected_files:
            raise RuntimeError('Running Compose file order differs from script invocation; stop')
        project_dir = self.files[0].parent
        if '--project-directory' in self.argv:
            project_dir = (self.cwd/self.argv[self.argv.index('--project-directory')+1]).resolve()
        if labels.get('com.docker.compose.project.working_dir') != str(project_dir):
            raise RuntimeError('Running Compose working directory differs; stop')
        if labels.get('com.docker.compose.project') != (self.project or resolved_project):
            raise RuntimeError('Running Compose project differs; stop')
        actual_env = tuple(Path(p).resolve() for p in labels.get('com.docker.compose.project.environment_file', '').split(',') if p)
        if actual_env != self.env_files:
            raise RuntimeError('Running Compose env-file provenance differs or is unavailable; stop')
