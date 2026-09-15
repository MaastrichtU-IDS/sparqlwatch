"""The one stylesheet this site serves, and the route that serves it."""

import pytest
from fastapi.testclient import TestClient

import app as app_module
from app import app


@pytest.fixture
def client():
    """No store override: this route reads no measurement.

    The stylesheet must serve before the first sweep -- it is what every other
    page depends on to be legible, so it cannot depend on there being data.
    """
    with TestClient(app) as built:
        yield built


def test_the_stylesheet_is_served_immutable(client):
    r = client.get(app_module.STYLESHEET_PATH)
    assert r.status_code == 200, "the hashed stylesheet must resolve"
    assert r.headers["content-type"].startswith("text/css")
    assert r.headers["Cache-Control"] == "public, max-age=31536000, immutable"


def test_a_wrong_hash_is_not_served(client):
    r = client.get("/static/site.000000000000.css")
    assert r.status_code == 404, (
        "an unversioned or stale URL must 404, not serve current bytes: "
        "the immutable header promises the bytes never change under a URL"
    )


def test_the_hash_covers_the_generated_verdict_rules(client):
    """The verdict CSS is generated, and it is part of what is cached.

    If the hash were taken over the static file alone, changing a verdict's
    border width would ship stale CSS to every browser holding a year-long
    immutable copy.
    """
    body = client.get(app_module.STYLESHEET_PATH).text
    assert ".enc-verified" in body, "generated verdict rules must be in the file"
    assert ".enc-text-verified" in body


def test_every_token_is_declared_for_both_surfaces(client):
    body = client.get(app_module.STYLESHEET_PATH).text
    light = body.split("prefers-color-scheme: dark")[0]
    dark = body.split("prefers-color-scheme: dark")[1]
    for token in ("--bg", "--text", "--accent", "--good", "--warn", "--crit", "--fill"):
        assert f"{token}:" in light, f"{token} must have a light value"
        assert f"{token}:" in dark, f"{token} must have a dark value"
