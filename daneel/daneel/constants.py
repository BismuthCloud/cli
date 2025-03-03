BIG_MODEL = "BIG_MODEL"
MODEL = "MODEL"
SMALL_MODEL = "SMALL_MODEL"

# TODO: pull from s3
# Provider -> Suite -> Model Class -> Model Name
MODEL_CONFIGURATION = {
    "openrouter": {
        "default": {
            "BIG_MODEL": "anthropic/claude-3.7-sonnet",
            "MODEL": "anthropic/claude-3.5-haiku",
            "SMALL_MODEL": "anthropic/claude-3-haiku",
        },
        "openai": {
            "BIG_MODEL": "openai/o3-mini",
            "MODEL": "openai/gpt-4o",
            "SMALL_MODEL": "openai/gpt-4o-mini",
        },
    },
    "anthropic": {
        "default": {
            "BIG_MODEL": "claude-3-7-sonnet-20250219",
            "MODEL": "claude-3-5-haiku-20241022",
            "SMALL_MODEL": "claude-3-5-haiku-20241022",
        },
        "thinking": {
            "BIG_MODEL": "claude-3-7-sonnet-20250219",
            "MODEL": "claude-3-5-haiku-20241022",
            "SMALL_MODEL": "claude-3-5-haiku-20241022",
        },
    },
    "bedrock": {
        "default": {
            "BIG_MODEL": "anthropic.claude-3-7-sonnet-20250219-v1:0",
            "MODEL": "anthropic.claude-3-5-haiku-20241022-v1:0",
            "SMALL_MODEL": "anthropic.claude-3-haiku-20240307-v1:0",
        }
    },
    "google": {
        "default": {
            "BIG_MODEL": "gemini-2.0-flash-exp",
            "MODEL": "gemini-2.0-flash-exp",
            "SMALL_MODEL": "gemini-1.5-flash",
        }
    },
}

LANGUAGE_CONSTRAINTS = """
Language and scenario specific constraints
   - Javascript/Typescript
      > UI Testing: 
         IMPORTANT - We are not writing or running tests for UI elements because testing a UI is a substantially different problem to building the code base.
         * Explicitly note that tests should not be written or run, and to focus on linting, compiling and static analysis for verifying correctness.
         * Consider that testing can cause complex feedback loops and should be handled at a different time unless the user has explicitly asked for a written test.
      > UI Linting:
         IMPORTANT - Not every linting error needs to be fixed for a UI, in an interactive setting its better to get the code working and let users determine whether or not they care about the munutia like those errors.
      > Chrome Extension Testing:
         IMPORTANT - Creating chrome extension tests are very tricky because of the need to mock chrome's APIs, if the user is building a chrome extension focus on getting the code working correctly through building the extension and fixing hard errors.
   - Python
        > Test Coverage:
                IMPORTANT - Python is a dynamically typed language, so it is important to have a good test coverage to ensure that the code is working as expected.
        > Linting:
                IMPORTANT - Linting is important in Python because it helps to maintain the code quality and consistency. Run mypy on any changed sources to ensure imports are correct.
        > Type Hints:
                IMPORTANT - Type hints are important in Python because they help to catch bugs early in the development process.
   - Rust
        > Test Running:
                IMPORTANT - Rust is a systems programming language, so it is important to have a good test coverage to ensure that the code is working as expected.
                Please ensure that the tests are passing and the code is compiling.
                Common test frameworks include: `cargo test`, `cargo test --release`, `cargo test --doc`. Choose the appropriate one based on the context.
        > Linting:
                IMPORTANT - Linting is important in Rust because it helps to maintain the code quality and consistency.
        > Clippy:
                IMPORTANT - Clippy is a tool that helps to catch common mistakes and improve the code quality.
"""
