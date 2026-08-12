"""Tests for PyPI-dep tool plugins (dns, whois, pdf_extract, xml,
html_extract, system_info, process_list, wikipedia, rss, webhook,
web_fetch, python_eval).

The plugins use lazy imports so each test injects a fake module via
``sys.modules`` to exercise execute() without requiring the real dep
to be installed in the dev environment. ``check()`` tests use
``importlib.util.find_spec`` mocking.
"""

from __future__ import annotations

import importlib.util
import json as _json
import sys
import types
from typing import Any

import pytest

from circuitry.plugins import build_plugin
from circuitry.plugins.dns import DnsPlugin
from circuitry.plugins.html_extract import HtmlExtractPlugin
from circuitry.plugins.pdf_extract import PdfExtractPlugin
from circuitry.plugins.process_list import ProcessListPlugin
from circuitry.plugins.python_eval import PythonEvalPlugin
from circuitry.plugins.rss import RssPlugin
from circuitry.plugins.system_info import SystemInfoPlugin
from circuitry.plugins.web_fetch import WebFetchPlugin
from circuitry.plugins.webhook import WebhookPlugin
from circuitry.plugins.whois import WhoisPlugin
from circuitry.plugins.wikipedia import WikipediaPlugin
from circuitry.plugins.xml import XmlPlugin

PYPI_PLUGINS = [
    "dns", "whois", "pdf_extract", "xml", "html_extract",
    "system_info", "process_list", "wikipedia", "rss",
    "webhook", "web_fetch", "python_eval",
]


@pytest.mark.parametrize("name", PYPI_PLUGINS)
def test_factory_builds_each_pypi_plugin(name: str) -> None:
    p = build_plugin(plugin_name=name, runtime={})
    assert p.name == name


# ---------- check() reports library:<dep> when dep is missing ----------


def _force_missing(monkeypatch: pytest.MonkeyPatch, *modules: str) -> None:
    real = importlib.util.find_spec

    def fake_find_spec(name: str, *args: Any, **kwargs: Any):
        if name in modules:
            return None
        return real(name, *args, **kwargs)

    monkeypatch.setattr("importlib.util.find_spec", fake_find_spec)


def test_dns_check_when_missing(monkeypatch: pytest.MonkeyPatch) -> None:
    _force_missing(monkeypatch, "dns")
    r = DnsPlugin().check()
    assert r.ok is False and "library:dnspython" in r.missing


def test_whois_check_when_missing(monkeypatch: pytest.MonkeyPatch) -> None:
    _force_missing(monkeypatch, "whois")
    r = WhoisPlugin().check()
    assert r.ok is False and "library:python-whois" in r.missing


def test_pdf_extract_check_when_missing(monkeypatch: pytest.MonkeyPatch) -> None:
    _force_missing(monkeypatch, "pdfplumber")
    r = PdfExtractPlugin().check()
    assert r.ok is False and "library:pdfplumber" in r.missing


def test_xml_check_when_missing(monkeypatch: pytest.MonkeyPatch) -> None:
    _force_missing(monkeypatch, "lxml")
    r = XmlPlugin().check()
    assert r.ok is False and "library:lxml" in r.missing


def test_html_extract_check_when_missing(monkeypatch: pytest.MonkeyPatch) -> None:
    _force_missing(monkeypatch, "bs4")
    r = HtmlExtractPlugin().check()
    assert r.ok is False and "library:beautifulsoup4" in r.missing


def test_wikipedia_check_when_missing(monkeypatch: pytest.MonkeyPatch) -> None:
    _force_missing(monkeypatch, "wikipediaapi")
    r = WikipediaPlugin().check()
    assert r.ok is False and "library:wikipedia-api" in r.missing


def test_rss_check_when_missing(monkeypatch: pytest.MonkeyPatch) -> None:
    _force_missing(monkeypatch, "feedparser")
    r = RssPlugin().check()
    assert r.ok is False and "library:feedparser" in r.missing


def test_webhook_check_when_missing(monkeypatch: pytest.MonkeyPatch) -> None:
    _force_missing(monkeypatch, "requests")
    r = WebhookPlugin().check()
    assert r.ok is False and "library:requests" in r.missing


def test_web_fetch_check_when_both_missing(monkeypatch: pytest.MonkeyPatch) -> None:
    _force_missing(monkeypatch, "requests", "trafilatura")
    r = WebFetchPlugin().check()
    assert r.ok is False
    assert "library:requests" in r.missing
    assert "library:trafilatura" in r.missing


def test_python_eval_check_when_missing(monkeypatch: pytest.MonkeyPatch) -> None:
    _force_missing(monkeypatch, "RestrictedPython")
    r = PythonEvalPlugin().check()
    assert r.ok is False and "library:RestrictedPython" in r.missing


# ---------- execute() with injected fake libraries ----------


def _install_fake_dns(monkeypatch: pytest.MonkeyPatch) -> dict[str, Any]:
    captured: dict[str, Any] = {}
    fake_dns = types.ModuleType("dns")
    fake_resolver_mod = types.ModuleType("dns.resolver")

    class FakeRecord:
        def __init__(self, text: str) -> None:
            self._text = text

        def __str__(self) -> str:
            return self._text

    class FakeAnswer:
        rrset = types.SimpleNamespace(ttl=300)

        def __init__(self, records: list[str]) -> None:
            self._records = [FakeRecord(r) for r in records]

        def __iter__(self):
            return iter(self._records)

    class FakeNXDOMAIN(Exception):
        pass

    class FakeNoAnswer(Exception):
        pass

    class FakeResolver:
        def __init__(self) -> None:
            captured["resolver_inited"] = True
            self.lifetime = 0.0
            self.nameservers: list[str] = []

        def resolve(self, domain: str, rdtype: str) -> Any:
            captured["domain"] = domain
            captured["rdtype"] = rdtype
            captured["nameservers"] = list(self.nameservers)
            return FakeAnswer(["1.2.3.4"])

    fake_resolver_mod.Resolver = FakeResolver
    fake_resolver_mod.NXDOMAIN = FakeNXDOMAIN
    fake_resolver_mod.NoAnswer = FakeNoAnswer
    fake_dns.resolver = fake_resolver_mod
    monkeypatch.setitem(sys.modules, "dns", fake_dns)
    monkeypatch.setitem(sys.modules, "dns.resolver", fake_resolver_mod)
    return captured


def test_dns_execute_with_fake_resolver(monkeypatch: pytest.MonkeyPatch) -> None:
    captured = _install_fake_dns(monkeypatch)
    r = DnsPlugin().execute(
        params={"domain": "example.com", "type": "A", "nameservers": ["1.1.1.1"]}
    )
    # FakeAnswer iterates objects whose str() returns the IP.
    assert "1.2.3.4" in str(r.value[0])
    assert captured["domain"] == "example.com"
    assert captured["rdtype"] == "A"
    assert captured["nameservers"] == ["1.1.1.1"]
    assert r.exit_code == 0


def test_dns_execute_nxdomain(monkeypatch: pytest.MonkeyPatch) -> None:
    fake_dns = types.ModuleType("dns")
    fake_resolver_mod = types.ModuleType("dns.resolver")

    class FakeNXDOMAIN(Exception):
        pass

    class FakeNoAnswer(Exception):
        pass

    class FakeResolver:
        def __init__(self) -> None:
            self.lifetime = 0.0
            self.nameservers: list[str] = []

        def resolve(self, *a: Any, **k: Any) -> Any:
            raise FakeNXDOMAIN()

    fake_resolver_mod.Resolver = FakeResolver
    fake_resolver_mod.NXDOMAIN = FakeNXDOMAIN
    fake_resolver_mod.NoAnswer = FakeNoAnswer
    fake_dns.resolver = fake_resolver_mod
    monkeypatch.setitem(sys.modules, "dns", fake_dns)
    monkeypatch.setitem(sys.modules, "dns.resolver", fake_resolver_mod)

    r = DnsPlugin().execute(params={"domain": "nonexistent.test", "type": "A"})
    assert r.value == []
    assert r.exit_code == 1


def test_whois_execute_normalises_dates(monkeypatch: pytest.MonkeyPatch) -> None:
    from datetime import datetime as _dt

    fake_whois = types.ModuleType("whois")

    class FakeEntry(dict):
        pass

    def fake_lookup(domain: str) -> FakeEntry:
        return FakeEntry(
            registrar="Example Registrar",
            creation_date=_dt(2020, 1, 1, 12, 0, 0),
            name_servers=["NS1.X.TEST", "NS2.X.TEST"],
        )

    fake_whois.whois = fake_lookup
    monkeypatch.setitem(sys.modules, "whois", fake_whois)

    r = WhoisPlugin().execute(params={"domain": "example.com"})
    assert r.value["registrar"] == "Example Registrar"
    # Datetime → ISO string.
    assert r.value["creation_date"].startswith("2020-01-01")


def test_pdf_extract_text_mode(monkeypatch: pytest.MonkeyPatch) -> None:
    fake_mod = types.ModuleType("pdfplumber")

    class FakePage:
        def __init__(self, text: str) -> None:
            self._text = text

        def extract_text(self) -> str:
            return self._text

        def extract_tables(self) -> list:
            return [[["a", "b"], ["1", "2"]]]

    class FakePdf:
        def __init__(self) -> None:
            self.pages = [FakePage("page one"), FakePage("page two")]

        def __enter__(self) -> FakePdf:
            return self

        def __exit__(self, *args: Any) -> None:
            return None

    fake_mod.open = lambda path: FakePdf()
    monkeypatch.setitem(sys.modules, "pdfplumber", fake_mod)

    r = PdfExtractPlugin().execute(params={"path": "/tmp/x.pdf"})
    assert r.value == "page one\npage two"

    r2 = PdfExtractPlugin().execute(
        params={"path": "/tmp/x.pdf", "mode": "per_page"}
    )
    assert r2.value == ["page one", "page two"]

    r3 = PdfExtractPlugin().execute(
        params={"path": "/tmp/x.pdf", "mode": "tables", "pages": [1]}
    )
    assert r3.value == [[[["a", "b"], ["1", "2"]]]]


def test_xml_xpath_with_namespaces(monkeypatch: pytest.MonkeyPatch) -> None:
    """Use real lxml if available, otherwise skip — xml is too coupled
    to lxml's API to mock cleanly."""
    if importlib.util.find_spec("lxml") is None:
        pytest.skip("lxml not installed")
    xml_text = "<root><a>x</a><b>y</b></root>"
    r = XmlPlugin().execute(
        params={"mode": "xpath", "input": xml_text, "xpath": "//a/text()"}
    )
    assert "x" in r.value


def test_xml_to_string_pretty(monkeypatch: pytest.MonkeyPatch) -> None:
    if importlib.util.find_spec("lxml") is None:
        pytest.skip("lxml not installed")
    r = XmlPlugin().execute(
        params={"mode": "to_string", "input": "<r><a/></r>", "pretty": True}
    )
    assert "<a/>" in r.value


def test_html_extract_text_via_real_bs4_if_available() -> None:
    if importlib.util.find_spec("bs4") is None:
        pytest.skip("beautifulsoup4 not installed")
    html = '<html><body><h1>Title</h1><p class="x">hi</p><p class="x">there</p></body></html>'
    r = HtmlExtractPlugin().execute(params={"input": html, "selector": "p.x"})
    assert r.value == ["hi", "there"]


def test_html_extract_attr_mode() -> None:
    if importlib.util.find_spec("bs4") is None:
        pytest.skip("beautifulsoup4 not installed")
    html = '<a href="https://x.test">link</a>'
    r = HtmlExtractPlugin().execute(
        params={"input": html, "selector": "a", "mode": "attr", "attribute": "href"}
    )
    assert r.value == ["https://x.test"]


def test_system_info_uses_psutil() -> None:
    """psutil is available in the dev env — exercise the real plugin."""
    r = SystemInfoPlugin().execute(params={"sections": ["cpu", "memory"]})
    assert "cpu" in r.value and "logical_cores" in r.value["cpu"]
    assert "memory" in r.value
    assert "disk" not in r.value  # filtered


def test_process_list_returns_rows() -> None:
    r = ProcessListPlugin().execute(params={"limit": 5})
    assert isinstance(r.value, list)
    assert len(r.value) <= 5
    if r.value:
        keys = set(r.value[0])
        assert {"pid", "name", "cpu_percent", "memory_percent"} <= keys


def test_process_list_invalid_sort() -> None:
    with pytest.raises(ValueError, match="sort_by"):
        ProcessListPlugin().execute(params={"sort_by": "ram"})


def test_wikipedia_summary_mode(monkeypatch: pytest.MonkeyPatch) -> None:
    fake_mod = types.ModuleType("wikipediaapi")
    captured: dict[str, Any] = {}

    class FakePage:
        def __init__(self, title: str) -> None:
            self._title = title
            self.summary = "Summary text."
            self.text = "Full article text."
            self.fullurl = f"https://en.wikipedia.org/wiki/{title}"
            self.sections: list = []

        def exists(self) -> bool:
            return True

    class FakeWiki:
        def __init__(self, *, user_agent: str, language: str) -> None:
            captured["user_agent"] = user_agent
            captured["language"] = language

        def page(self, title: str) -> FakePage:
            captured["title"] = title
            return FakePage(title)

    fake_mod.Wikipedia = FakeWiki
    monkeypatch.setitem(sys.modules, "wikipediaapi", fake_mod)

    r = WikipediaPlugin().execute(params={"title": "YAML", "language": "en"})
    assert r.value == "Summary text."
    assert captured["title"] == "YAML"
    assert "circuitry" in captured["user_agent"]


def test_wikipedia_missing_page(monkeypatch: pytest.MonkeyPatch) -> None:
    fake_mod = types.ModuleType("wikipediaapi")

    class FakePage:
        def exists(self) -> bool:
            return False

    class FakeWiki:
        def __init__(self, **kwargs: Any) -> None:
            pass

        def page(self, title: str) -> FakePage:
            return FakePage()

    fake_mod.Wikipedia = FakeWiki
    monkeypatch.setitem(sys.modules, "wikipediaapi", fake_mod)

    r = WikipediaPlugin().execute(params={"title": "DefinitelyNotAnArticle"})
    assert r.value is None
    assert r.exit_code == 1


def test_rss_parses_feed(monkeypatch: pytest.MonkeyPatch) -> None:
    fake_mod = types.ModuleType("feedparser")

    def fake_parse(url: str) -> Any:
        return types.SimpleNamespace(
            bozo=False,
            feed=types.SimpleNamespace(title="Example Feed"),
            entries=[
                types.SimpleNamespace(
                    title="Post 1", link="https://x.test/1",
                    summary="s1", published="2026-01-01",
                    author="alice", id="g1",
                ),
                types.SimpleNamespace(
                    title="Post 2", link="https://x.test/2",
                    summary="s2", published="2026-01-02",
                    author="bob", id="g2",
                ),
            ],
        )

    fake_mod.parse = fake_parse
    monkeypatch.setitem(sys.modules, "feedparser", fake_mod)

    r = RssPlugin().execute(params={"url": "https://x.test/feed", "limit": 1})
    assert len(r.value) == 1
    assert r.value[0]["title"] == "Post 1"
    assert r.raw["feed_title"] == "Example Feed"


def test_webhook_posts_json_with_mocked_requests(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    fake_mod = types.ModuleType("requests")
    fake_exc_mod = types.ModuleType("requests.exceptions")

    class FakeRequestException(Exception):
        pass

    fake_exc_mod.RequestException = FakeRequestException
    fake_mod.exceptions = fake_exc_mod

    captured: dict[str, Any] = {}

    class FakeResponse:
        def __init__(self) -> None:
            self.status_code = 200
            self.text = _json.dumps({"ok": True})
            self.headers = {"Content-Type": "application/json"}

    def fake_request(**kwargs: Any) -> FakeResponse:
        captured.update(kwargs)
        return FakeResponse()

    fake_mod.request = fake_request
    monkeypatch.setitem(sys.modules, "requests", fake_mod)
    monkeypatch.setitem(sys.modules, "requests.exceptions", fake_exc_mod)

    r = WebhookPlugin().execute(
        params={
            "url": "https://hooks.test/x",
            "json": {"a": 1},
            "headers": {"X-Token": "y"},
        }
    )
    assert r.value == {"ok": True}
    assert r.exit_code == 200
    assert captured["method"] == "POST"
    assert captured["json"] == {"a": 1}
    assert captured["headers"] == {"X-Token": "y"}


def test_webhook_5xx_retry_then_succeed(monkeypatch: pytest.MonkeyPatch) -> None:
    fake_mod = types.ModuleType("requests")
    fake_exc_mod = types.ModuleType("requests.exceptions")
    fake_exc_mod.RequestException = type("RequestException", (Exception,), {})
    fake_mod.exceptions = fake_exc_mod

    statuses = [500, 502, 200]
    calls: list[int] = []

    class R:
        def __init__(self, status: int) -> None:
            self.status_code = status
            self.text = "ok" if status == 200 else "fail"
            self.headers = {"Content-Type": "text/plain"}

    def fake_request(**kwargs: Any) -> R:
        s = statuses[len(calls)]
        calls.append(s)
        return R(s)

    fake_mod.request = fake_request
    monkeypatch.setitem(sys.modules, "requests", fake_mod)
    monkeypatch.setitem(sys.modules, "requests.exceptions", fake_exc_mod)
    # Speed up retries.
    monkeypatch.setattr("circuitry.plugins.webhook.time.sleep", lambda _: None)

    r = WebhookPlugin().execute(
        params={"url": "https://x.test", "retries": 2}
    )
    assert r.exit_code == 200
    assert calls == [500, 502, 200]


def test_web_fetch_html_mode(monkeypatch: pytest.MonkeyPatch) -> None:
    fake_mod = types.ModuleType("requests")
    fake_exc_mod = types.ModuleType("requests.exceptions")
    fake_exc_mod.RequestException = type("RequestException", (Exception,), {})
    fake_mod.exceptions = fake_exc_mod

    class FakeResponse:
        status_code = 200
        text = "<html><body>raw</body></html>"
        headers = {"Content-Type": "text/html"}

    fake_mod.get = lambda *a, **k: FakeResponse()
    monkeypatch.setitem(sys.modules, "requests", fake_mod)
    monkeypatch.setitem(sys.modules, "requests.exceptions", fake_exc_mod)

    r = WebFetchPlugin().execute(
        params={"url": "https://x.test", "mode": "html"}
    )
    assert r.value == "<html><body>raw</body></html>"
    assert r.exit_code == 200


def test_web_fetch_text_via_trafilatura(monkeypatch: pytest.MonkeyPatch) -> None:
    fake_mod = types.ModuleType("requests")
    fake_exc_mod = types.ModuleType("requests.exceptions")
    fake_exc_mod.RequestException = type("RequestException", (Exception,), {})
    fake_mod.exceptions = fake_exc_mod

    class FakeResponse:
        status_code = 200
        text = "<html><body><nav>x</nav><article>main body</article></body></html>"
        headers = {"Content-Type": "text/html"}

    fake_mod.get = lambda *a, **k: FakeResponse()
    monkeypatch.setitem(sys.modules, "requests", fake_mod)
    monkeypatch.setitem(sys.modules, "requests.exceptions", fake_exc_mod)

    fake_traf = types.ModuleType("trafilatura")
    fake_traf.extract = lambda html, **k: "main body"
    monkeypatch.setitem(sys.modules, "trafilatura", fake_traf)

    r = WebFetchPlugin().execute(params={"url": "https://x.test"})
    assert r.value == "main body"


def test_web_fetch_json_mode(monkeypatch: pytest.MonkeyPatch) -> None:
    fake_mod = types.ModuleType("requests")
    fake_exc_mod = types.ModuleType("requests.exceptions")
    fake_exc_mod.RequestException = type("RequestException", (Exception,), {})
    fake_mod.exceptions = fake_exc_mod

    class FakeResponse:
        status_code = 200
        text = _json.dumps({"a": 1})
        headers = {"Content-Type": "application/json"}

    fake_mod.get = lambda *a, **k: FakeResponse()
    monkeypatch.setitem(sys.modules, "requests", fake_mod)
    monkeypatch.setitem(sys.modules, "requests.exceptions", fake_exc_mod)

    r = WebFetchPlugin().execute(
        params={"url": "https://x.test", "mode": "json"}
    )
    assert r.value == {"a": 1}


def test_python_eval_evaluates_simple_expression() -> None:
    if importlib.util.find_spec("RestrictedPython") is None:
        pytest.skip("RestrictedPython not installed")
    r = PythonEvalPlugin().execute(
        params={"code": "1 + sum(xs)", "inputs": {"xs": [1, 2, 3]}}
    )
    assert r.value == 7


def test_python_eval_exec_mode_reads_result_var() -> None:
    if importlib.util.find_spec("RestrictedPython") is None:
        pytest.skip("RestrictedPython not installed")
    r = PythonEvalPlugin().execute(
        params={
            "mode": "exec",
            "code": "result = sum([1, 2, 3])",
        }
    )
    assert r.value == 6


def test_python_eval_rejects_dunder_input_names() -> None:
    """Validation runs before any compile/exec, regardless of dep
    presence — input names starting with underscore are rejected."""
    with pytest.raises(ValueError, match="cannot start with underscore"):
        PythonEvalPlugin().execute(
            params={"code": "_x", "inputs": {"_x": 1}}
        )


def test_python_eval_rejects_imports_via_sandbox() -> None:
    """AC C.5: ``import os`` rejected by RestrictedPython before exec."""
    if importlib.util.find_spec("RestrictedPython") is None:
        pytest.skip("RestrictedPython not installed")
    with pytest.raises(PermissionError, match="rejected by sandbox"):
        PythonEvalPlugin().execute(
            params={"code": "import os; os.system('echo bad')"}
        )
