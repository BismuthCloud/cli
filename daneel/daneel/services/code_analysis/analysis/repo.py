from typing import Dict, Mapping
from pathlib import Path
from .source_file import SourceFile, UnknownExtensionException


class Repository(object):
    files: Dict[str, SourceFile]
    
    def __init__(self, files: Mapping[str, str | bytes]):
        self.files = {}
        for filename, contents in files.items():
            try:
                self.files[filename] = SourceFile(filename, contents if isinstance(contents, bytes) else contents.encode('utf-8'))
            except UnknownExtensionException:
                print(f"code analysis: unparsable extension for {filename}")
                continue

    @staticmethod
    def from_fs(dir: Path) -> 'Repository':
        r = Repository({})
        for path in dir.glob('**/*'):
            if path.is_file():
                with open(path, 'rb') as f:
                    r.files[path.relative_to(dir)] = f.read()
        return r