"""Tests for SDK / cloud / browser / ML tool plugins.

Each plugin uses lazy imports inside ``execute()``, so tests inject
fake SDK modules via ``sys.modules`` to drive the happy path without
requiring (often heavy) dev installs of slack-sdk / boto3 /
sentence-transformers / etc.
"""

from __future__ import annotations

import importlib.util
import sys
import types
from typing import Any

import pytest

from circuitry.plugins import build_plugin
from circuitry.plugins.discord import DiscordPlugin
from circuitry.plugins.embed import _MODEL_CACHE as _EMBED_CACHE
from circuitry.plugins.embed import EmbedPlugin
from circuitry.plugins.gcalendar import GCalendarPlugin
from circuitry.plugins.gdrive import GDrivePlugin
from circuitry.plugins.github import GitHubPlugin
from circuitry.plugins.jira import JiraPlugin
from circuitry.plugins.linear import LinearPlugin
from circuitry.plugins.notion import NotionPlugin
from circuitry.plugins.playwright import PlaywrightPlugin
from circuitry.plugins.rerank import _MODEL_CACHE as _RERANK_CACHE
from circuitry.plugins.rerank import RerankPlugin
from circuitry.plugins.s3_tool import S3ToolPlugin
from circuitry.plugins.screenshot import ScreenshotPlugin
from circuitry.plugins.slack import SlackPlugin
from circuitry.plugins.vector_search import VectorSearchPlugin

SDK_PLUGINS = [
    "linear", "slack", "discord", "github", "jira", "notion",
    "gcalendar", "gdrive", "s3", "playwright", "screenshot",
    "embed", "rerank", "vector_search",
]


@pytest.mark.parametrize("name", SDK_PLUGINS)
def test_factory_builds_each_sdk_plugin(name: str) -> None:
    p = build_plugin(plugin_name=name, runtime={})
    assert p.name == name


# ---------------------------------------------------------------------------
# check() reports missing deps + missing env vars
# ---------------------------------------------------------------------------


def _force_missing(monkeypatch: pytest.MonkeyPatch, *modules: str) -> None:
    real = importlib.util.find_spec

    def fake_find_spec(name: str, *args: Any, **kwargs: Any):
        if name in modules:
            return None
        return real(name, *args, **kwargs)

    monkeypatch.setattr("importlib.util.find_spec", fake_find_spec)


def test_linear_check_missing_env(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("LINEAR_API_KEY", raising=False)
    r = LinearPlugin().check()
    assert "env:LINEAR_API_KEY" in r.missing


def test_slack_check_reports_lib_and_env(monkeypatch: pytest.MonkeyPatch) -> None:
    _force_missing(monkeypatch, "slack_sdk")
    monkeypatch.delenv("SLACK_BOT_TOKEN", raising=False)
    r = SlackPlugin().check()
    assert "library:slack-sdk" in r.missing
    assert "env:SLACK_BOT_TOKEN" in r.missing


def test_discord_check_when_missing(monkeypatch: pytest.MonkeyPatch) -> None:
    _force_missing(monkeypatch, "discord")
    r = DiscordPlugin().check()
    assert "library:discord.py" in r.missing


def test_github_check_when_missing(monkeypatch: pytest.MonkeyPatch) -> None:
    _force_missing(monkeypatch, "github")
    r = GitHubPlugin().check()
    assert "library:PyGithub" in r.missing


def test_jira_check_reports_all(monkeypatch: pytest.MonkeyPatch) -> None:
    _force_missing(monkeypatch, "atlassian")
    for env in ("JIRA_URL", "JIRA_USER", "JIRA_TOKEN"):
        monkeypatch.delenv(env, raising=False)
    r = JiraPlugin().check()
    assert "library:atlassian-python-api" in r.missing
    assert "env:JIRA_URL" in r.missing
    assert "env:JIRA_USER" in r.missing
    assert "env:JIRA_TOKEN" in r.missing


def test_notion_check_when_missing(monkeypatch: pytest.MonkeyPatch) -> None:
    _force_missing(monkeypatch, "notion_client")
    monkeypatch.delenv("NOTION_TOKEN", raising=False)
    r = NotionPlugin().check()
    assert "library:notion-client" in r.missing
    assert "env:NOTION_TOKEN" in r.missing


def test_gcalendar_check_handles_namespace_package(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """When the ``google`` namespace package isn't installed, find_spec
    raises ModuleNotFoundError. The plugin must catch this gracefully."""
    _force_missing(monkeypatch, "googleapiclient")
    monkeypatch.delenv("GOOGLE_APPLICATION_CREDENTIALS", raising=False)
    r = GCalendarPlugin().check()
    assert "library:google-api-python-client" in r.missing
    # google-auth flagged whether the namespace package raises or
    # returns None — both produce the same missing-marker result.
    assert "library:google-auth" in r.missing


def test_gdrive_check_handles_namespace_package(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _force_missing(monkeypatch, "googleapiclient")
    monkeypatch.delenv("GOOGLE_APPLICATION_CREDENTIALS", raising=False)
    r = GDrivePlugin().check()
    assert "library:google-api-python-client" in r.missing
    assert "library:google-auth" in r.missing


def test_s3_tool_check_when_missing(monkeypatch: pytest.MonkeyPatch) -> None:
    _force_missing(monkeypatch, "boto3")
    r = S3ToolPlugin().check()
    assert "library:boto3" in r.missing


def test_playwright_check_when_missing(monkeypatch: pytest.MonkeyPatch) -> None:
    _force_missing(monkeypatch, "playwright")
    r = PlaywrightPlugin().check()
    assert "library:playwright" in r.missing


def test_screenshot_check_when_missing(monkeypatch: pytest.MonkeyPatch) -> None:
    _force_missing(monkeypatch, "playwright")
    r = ScreenshotPlugin().check()
    assert "library:playwright" in r.missing


def test_embed_check_when_missing(monkeypatch: pytest.MonkeyPatch) -> None:
    _force_missing(monkeypatch, "sentence_transformers")
    r = EmbedPlugin().check()
    assert "library:sentence-transformers" in r.missing


def test_rerank_check_when_missing(monkeypatch: pytest.MonkeyPatch) -> None:
    _force_missing(monkeypatch, "sentence_transformers")
    r = RerankPlugin().check()
    assert "library:sentence-transformers" in r.missing


def test_vector_search_check_when_missing(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _force_missing(monkeypatch, "chromadb")
    r = VectorSearchPlugin().check()
    assert "library:chromadb" in r.missing


# ---------------------------------------------------------------------------
# execute() with injected fake SDKs
# ---------------------------------------------------------------------------


def test_linear_list_issues_via_fake_requests(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("LINEAR_API_KEY", "lin_key")
    fake_mod = types.ModuleType("requests")
    fake_exc_mod = types.ModuleType("requests.exceptions")
    fake_exc_mod.RequestException = type("RequestException", (Exception,), {})
    fake_mod.exceptions = fake_exc_mod

    captured: dict[str, Any] = {}

    class FakeResponse:
        status_code = 200

        def json(self) -> dict[str, Any]:
            return {
                "data": {"issues": {"nodes": [{"id": "I1", "title": "Test"}]}}
            }

        @property
        def text(self) -> str:
            return ""

    def fake_post(url: str, **kwargs: Any) -> FakeResponse:
        captured["url"] = url
        captured["headers"] = kwargs.get("headers")
        captured["body"] = kwargs.get("json")
        return FakeResponse()

    fake_mod.post = fake_post
    monkeypatch.setitem(sys.modules, "requests", fake_mod)
    monkeypatch.setitem(sys.modules, "requests.exceptions", fake_exc_mod)

    r = LinearPlugin().execute(
        params={"mode": "list_issues", "team": "ENG", "state": "In Progress"}
    )
    assert r.value == [{"id": "I1", "title": "Test"}]
    assert captured["url"] == "https://api.linear.app/graphql"
    assert captured["headers"]["Authorization"] == "lin_key"
    assert captured["body"]["variables"]["filter"]["team"]["key"]["eq"] == "ENG"


def test_slack_post_message_with_fake_client(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("SLACK_BOT_TOKEN", "xoxb-1")
    fake_mod = types.ModuleType("slack_sdk")
    captured: dict[str, Any] = {}

    class FakeResponse:
        def __init__(self, data: dict[str, Any]) -> None:
            self.data = data

    class FakeWebClient:
        def __init__(self, *, token: str, timeout: int) -> None:
            captured["token"] = token

        def chat_postMessage(self, **kwargs: Any) -> FakeResponse:
            captured["chat_args"] = kwargs
            return FakeResponse({"ok": True, "ts": "1.0", "channel": kwargs["channel"]})

    fake_mod.WebClient = FakeWebClient
    monkeypatch.setitem(sys.modules, "slack_sdk", fake_mod)

    r = SlackPlugin().execute(
        params={"mode": "post_message", "channel": "#general", "text": "hi"}
    )
    assert r.value == {"ts": "1.0", "channel": "#general"}
    assert captured["token"] == "xoxb-1"
    assert captured["chat_args"]["text"] == "hi"


def test_discord_send_via_fake_webhook(monkeypatch: pytest.MonkeyPatch) -> None:
    fake_mod = types.ModuleType("discord")
    captured: dict[str, Any] = {}

    class FakeMessage:
        id = 12345
        channel_id = 99

    class FakeWebhook:
        @classmethod
        def from_url(cls, url: str) -> FakeWebhook:
            captured["url"] = url
            return cls()

        def send(self, **kwargs: Any) -> FakeMessage:
            captured["send_args"] = kwargs
            return FakeMessage()

    fake_mod.SyncWebhook = FakeWebhook
    monkeypatch.setitem(sys.modules, "discord", fake_mod)

    r = DiscordPlugin().execute(
        params={
            "webhook_url": "https://discord.com/api/webhooks/X/Y",
            "content": "hi",
            "username": "BotName",
        }
    )
    assert r.value["id"] == "12345"
    assert captured["send_args"]["content"] == "hi"
    assert captured["send_args"]["username"] == "BotName"


def test_github_get_repo_via_fake_pygithub(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    fake_mod = types.ModuleType("github")
    captured: dict[str, Any] = {}

    class FakeRepo:
        name = "circuitry"
        full_name = "kenan/circuitry"
        description = "Cybernetic orchestration"
        stargazers_count = 100
        forks_count = 5
        open_issues_count = 3
        default_branch = "main"
        html_url = "https://github.com/kenan/circuitry"

    class FakeGithub:
        def __init__(self, *args: Any, **kwargs: Any) -> None:
            captured["init_args"] = args

        def get_repo(self, slug: str) -> FakeRepo:
            captured["repo_slug"] = slug
            return FakeRepo()

    fake_mod.Github = FakeGithub
    monkeypatch.setitem(sys.modules, "github", fake_mod)
    monkeypatch.setenv("GITHUB_TOKEN", "ghp_x")

    r = GitHubPlugin().execute(params={"repo": "kenan/circuitry"})
    assert r.value["full_name"] == "kenan/circuitry"
    assert r.value["stars"] == 100
    assert captured["repo_slug"] == "kenan/circuitry"


def test_github_rejects_invalid_repo_format() -> None:
    with pytest.raises(ValueError, match="owner/name"):
        GitHubPlugin().execute(params={"repo": "noslash"})


def test_jira_get_issue_via_fake_atlassian(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    fake_atl = types.ModuleType("atlassian")

    class FakeJira:
        def __init__(self, **kwargs: Any) -> None:
            self._init = kwargs

        def issue(self, key: str) -> dict[str, Any]:
            return {"key": key, "fields": {"summary": "test"}}

    fake_atl.Jira = FakeJira
    monkeypatch.setitem(sys.modules, "atlassian", fake_atl)

    monkeypatch.setenv("JIRA_URL", "https://x.atlassian.net")
    monkeypatch.setenv("JIRA_USER", "user@x")
    monkeypatch.setenv("JIRA_TOKEN", "tok")

    r = JiraPlugin().execute(params={"mode": "get_issue", "key": "ENG-1"})
    assert r.value["key"] == "ENG-1"


def test_notion_query_database_via_fake_sdk(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    fake_mod = types.ModuleType("notion_client")

    class FakeDatabases:
        def query(self, **kwargs: Any) -> dict[str, Any]:
            return {"results": [{"id": "p1"}, {"id": "p2"}]}

    class FakeClient:
        def __init__(self, **kwargs: Any) -> None:
            self.databases = FakeDatabases()

    fake_mod.Client = FakeClient
    monkeypatch.setitem(sys.modules, "notion_client", fake_mod)
    monkeypatch.setenv("NOTION_TOKEN", "n_tok")

    r = NotionPlugin().execute(
        params={"mode": "query_database", "database_id": "abc"}
    )
    assert len(r.value) == 2


def test_gcalendar_list_events_via_fake_google(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Any,
) -> None:
    creds_file = tmp_path / "creds.json"
    creds_file.write_text("{}")
    monkeypatch.setenv("GOOGLE_APPLICATION_CREDENTIALS", str(creds_file))

    fake_googleapiclient = types.ModuleType("googleapiclient")
    fake_discovery = types.ModuleType("googleapiclient.discovery")

    fake_google = types.ModuleType("google")
    fake_oauth2 = types.ModuleType("google.oauth2")
    fake_service_account = types.ModuleType("google.oauth2.service_account")

    class FakeCreds:
        @classmethod
        def from_service_account_file(cls, path: str, scopes: Any) -> FakeCreds:
            return cls()

    fake_service_account.Credentials = FakeCreds
    fake_oauth2.service_account = fake_service_account
    fake_google.oauth2 = fake_oauth2

    class FakeEventsList:
        def execute(self) -> dict[str, Any]:
            return {"items": [{"id": "evt1"}, {"id": "evt2"}]}

    class FakeEvents:
        def list(self, **kwargs: Any) -> FakeEventsList:
            return FakeEventsList()

    class FakeService:
        def events(self) -> FakeEvents:
            return FakeEvents()

    def fake_build(*args: Any, **kwargs: Any) -> FakeService:
        return FakeService()

    fake_discovery.build = fake_build
    fake_googleapiclient.discovery = fake_discovery

    monkeypatch.setitem(sys.modules, "googleapiclient", fake_googleapiclient)
    monkeypatch.setitem(sys.modules, "googleapiclient.discovery", fake_discovery)
    monkeypatch.setitem(sys.modules, "google", fake_google)
    monkeypatch.setitem(sys.modules, "google.oauth2", fake_oauth2)
    monkeypatch.setitem(
        sys.modules, "google.oauth2.service_account", fake_service_account
    )

    r = GCalendarPlugin().execute(params={"mode": "list_events"})
    assert len(r.value) == 2


def test_s3_tool_list_via_fake_boto3(monkeypatch: pytest.MonkeyPatch) -> None:
    fake_mod = types.ModuleType("boto3")
    fake_botocore_config = types.ModuleType("botocore.config")
    fake_botocore_config.Config = lambda **kwargs: kwargs

    class FakeS3Client:
        def list_objects_v2(self, **kwargs: Any) -> dict[str, Any]:
            return {
                "Contents": [
                    {
                        "Key": "a.txt", "Size": 10, "ETag": "abc",
                        "LastModified": "2026-01-01",
                    }
                ]
            }

    fake_mod.client = lambda service, **kwargs: FakeS3Client()
    monkeypatch.setitem(sys.modules, "boto3", fake_mod)
    monkeypatch.setitem(sys.modules, "botocore.config", fake_botocore_config)

    r = S3ToolPlugin().execute(params={"bucket": "my-bucket", "mode": "list"})
    assert len(r.value) == 1
    assert r.value[0]["key"] == "a.txt"


def test_playwright_text_mode_via_fake_sdk(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    captured: dict[str, Any] = {}

    fake_mod = types.ModuleType("playwright")
    fake_sync_api = types.ModuleType("playwright.sync_api")

    class FakePage:
        url = "https://x.test/"

        def goto(self, url: str) -> None:
            captured["url"] = url

        def set_default_timeout(self, ms: int) -> None:
            pass

        def inner_text(self, selector: str) -> str:
            return "page text"

    class FakeContext:
        def new_page(self) -> FakePage:
            return FakePage()

    class FakeBrowser:
        def new_context(self, **kwargs: Any) -> FakeContext:
            return FakeContext()

        def close(self) -> None:
            captured["closed"] = True

    class FakeBrowserType:
        def launch(self, **kwargs: Any) -> FakeBrowser:
            captured["headless"] = kwargs.get("headless")
            return FakeBrowser()

    class FakeSyncContext:
        def __init__(self) -> None:
            self.chromium = FakeBrowserType()
            self.firefox = FakeBrowserType()
            self.webkit = FakeBrowserType()

        def __enter__(self) -> FakeSyncContext:
            return self

        def __exit__(self, *args: Any) -> None:
            return None

    fake_sync_api.sync_playwright = lambda: FakeSyncContext()
    fake_mod.sync_api = fake_sync_api
    monkeypatch.setitem(sys.modules, "playwright", fake_mod)
    monkeypatch.setitem(sys.modules, "playwright.sync_api", fake_sync_api)

    r = PlaywrightPlugin().execute(params={"url": "https://x.test/"})
    assert r.value == "page text"
    assert captured["url"] == "https://x.test/"
    assert captured["closed"] is True


def test_embed_via_fake_sentence_transformers(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _EMBED_CACHE.clear()
    fake_mod = types.ModuleType("sentence_transformers")

    class FakeST:
        def __init__(self, name: str, device: Any = None) -> None:
            self.name = name

        def encode(self, texts: list[str], **kwargs: Any) -> list[list[float]]:
            return [[0.1, 0.2, 0.3] for _ in texts]

    fake_mod.SentenceTransformer = FakeST
    monkeypatch.setitem(sys.modules, "sentence_transformers", fake_mod)

    r = EmbedPlugin().execute(params={"input": ["hello", "world"]})
    assert len(r.value) == 2
    assert r.value[0] == [0.1, 0.2, 0.3]
    assert r.raw["dimensions"] == 3


def test_embed_caches_model(monkeypatch: pytest.MonkeyPatch) -> None:
    _EMBED_CACHE.clear()
    fake_mod = types.ModuleType("sentence_transformers")
    init_calls: list[int] = []

    class FakeST:
        def __init__(self, name: str, device: Any = None) -> None:
            init_calls.append(1)

        def encode(self, texts: list[str], **kwargs: Any) -> list[list[float]]:
            return [[1.0] for _ in texts]

    fake_mod.SentenceTransformer = FakeST
    monkeypatch.setitem(sys.modules, "sentence_transformers", fake_mod)

    EmbedPlugin().execute(params={"input": "a"})
    EmbedPlugin().execute(params={"input": "b"})
    assert len(init_calls) == 1  # cached after first call


def test_rerank_sorts_by_score_descending(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _RERANK_CACHE.clear()
    fake_mod = types.ModuleType("sentence_transformers")

    class FakeCE:
        def __init__(self, name: str, device: Any = None) -> None:
            pass

        def predict(self, pairs: list[tuple[str, str]]) -> list[float]:
            # Mock scores — second candidate ranked highest.
            return [0.1, 0.9, 0.5]

    fake_mod.CrossEncoder = FakeCE
    monkeypatch.setitem(sys.modules, "sentence_transformers", fake_mod)

    r = RerankPlugin().execute(
        params={
            "query": "what is yaml",
            "candidates": ["c1", "c2", "c3"],
        }
    )
    scores = [item["score"] for item in r.value]
    assert scores == sorted(scores, reverse=True)
    assert r.value[0]["text"] == "c2"


def test_rerank_top_k_caps_results(monkeypatch: pytest.MonkeyPatch) -> None:
    _RERANK_CACHE.clear()
    fake_mod = types.ModuleType("sentence_transformers")

    class FakeCE:
        def __init__(self, *a: Any, **k: Any) -> None: pass
        def predict(self, pairs: list) -> list[float]:
            return list(range(len(pairs), 0, -1))

    fake_mod.CrossEncoder = FakeCE
    monkeypatch.setitem(sys.modules, "sentence_transformers", fake_mod)

    r = RerankPlugin().execute(
        params={"query": "q", "candidates": ["a", "b", "c", "d"], "top_k": 2}
    )
    assert len(r.value) == 2


def test_vector_search_query_via_fake_chromadb(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Any
) -> None:
    fake_mod = types.ModuleType("chromadb")

    class FakeCollection:
        def query(self, **kwargs: Any) -> dict[str, Any]:
            return {
                "ids": [["1", "2"]],
                "documents": [["doc one", "doc two"]],
                "metadatas": [[None, None]],
                "distances": [[0.1, 0.5]],
            }

        def add(self, **kwargs: Any) -> None:
            pass

        def count(self) -> int:
            return 7

        def delete(self, **kwargs: Any) -> None:
            pass

    class FakeClient:
        def get_or_create_collection(self, name: str) -> FakeCollection:
            return FakeCollection()

    fake_mod.PersistentClient = lambda path: FakeClient()
    monkeypatch.setitem(sys.modules, "chromadb", fake_mod)

    r = VectorSearchPlugin().execute(
        params={
            "mode": "query",
            "collection": "docs",
            "query_text": "yaml",
            "persist_path": str(tmp_path / "chroma"),
        }
    )
    assert len(r.value) == 2
    assert r.value[0]["id"] == "1"
    assert r.value[0]["distance"] == 0.1
