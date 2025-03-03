from daneel.data.graph_rag import GraphRag, KGNodeType, create_tables

create_tables()

graph = GraphRag(org_id=1, project_id=1, feature_id=1)


graph.upsert(
    "__init__",
    file_name="a/b/c/file_1.py",
    line=1,
    type=KGNodeType.FUNCTION,
    content="""
    def __init__(
        self, filename: str, contents: bytes, lang: Optional[Type[Language]] = None
    ):
        self.filename = filename
        self.contents = contents
        if lang is None:
            for ext, lang_cls in self.EXT_MAP.items():
                if filename.endswith(ext):
                    self.lang = lang_cls(self)
                    break
            else:
                raise UnknownExtensionException()
        else:
            self.lang = lang(self)

        self._tree = None
""",
)


graph.upsert(
    "main",
    file_name="a/b/c/file_2.py",
    line=1,
    type=KGNodeType.FUNCTION,
    content="""
  async def main():
    async with aiohttp.ClientSession() as session:
        async with session.get(
            os.environ.get("KEYCLOAK_URL", "http://localhost:8543/realms/bismuth")
            + "/protocol/openid-connect/certs"
        ) as response:
            jwks = jwt.PyJWKSet.from_dict(await response.json())
    handler = WebSocketHandler(jwks)
    server = await websockets.serve(
        handler.handle_client, "0.0.0.0", 8765, max_size=10000000
    )
    print("WebSocket server started on ws://0.0.0.0:8765")
    await server.wait_closed() 
""",
)


graph.upsert(
    "SourceFile",
    "a/b/c/file_1.py",
    line=1,
    type=KGNodeType.CLASS,
    content="""
class SourceFile(object):
    EXT_MAP = {
        ".py": Python,
        ".js": JavaScript,
        ".jsx": JavaScript,
        ".ts": TypeScript,
        ".tsx": TSX,
        ".toml": TOML,
        ".md": Markdown,
        ".json": JSON,
        ".yaml": YAML,
    }

    filename: str
    lang: Language
    contents: bytes
    _tree: Optional[tree_sitter.Tree]
""",
    edges=[0],
)


files = graph.search(
    "Okay are you aware of the source file? | I want to update the init method",
    graph_radius=1,
)
