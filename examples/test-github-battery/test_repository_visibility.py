import importlib.util
import json
from pathlib import Path
import unittest


SPEC = importlib.util.spec_from_file_location(
    "repository_visibility",
    Path(__file__).with_name("repository-visibility.py"),
)
assert SPEC is not None and SPEC.loader is not None
resolver = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(resolver)


class Response:
    def __init__(self, private: bool) -> None:
        self.body = json.dumps({"private": private}).encode()

    def __enter__(self) -> "Response":
        return self

    def __exit__(self, *_args: object) -> None:
        return None

    def read(self, _limit: int) -> bytes:
        return self.body


class RepositoryVisibilityTests(unittest.TestCase):
    def test_public_repository_produces_public_suspicious_data(self) -> None:
        private = resolver.repository_is_private(
            "archestra-ai", "OpenAPPA", "token", lambda *_args, **_kwargs: Response(False)
        )
        self.assertEqual(
            resolver.annotation(private)["answer"]["delta"],
            {"trust": "suspicious", "audience": "public"},
        )

    def test_private_repository_produces_internal_suspicious_data(self) -> None:
        private = resolver.repository_is_private(
            "example", "private", "token", lambda *_args, **_kwargs: Response(True)
        )
        self.assertEqual(
            resolver.annotation(private)["answer"]["delta"],
            {"trust": "suspicious", "audience": ["github:internal"]},
        )


if __name__ == "__main__":
    unittest.main()
