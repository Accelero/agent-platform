"""The installed Hermes image, started as a Pod starts it: non-root, a read-only
root, no network and only scoped mounts. The image under test is named by
`HERMES_TEST_NATIVE_IMAGE` (the `python-native` profile)."""

import json
import textwrap

import pytest

from tests.support.native import (
    ISSUER,
    PYTHON,
    CommandFailed,
    Instance,
    Layout,
    docker,
    image,
)
from tests.support.ownership import Cleanup

pytestmark = [pytest.mark.integration, pytest.mark.native]

HEALTH = textwrap.dedent(
    """
    import urllib.request
    assert urllib.request.urlopen(
        'http://127.0.0.1:9119/api/health', timeout=3).status == 200
    """
)


def script(text):
    return textwrap.dedent(text)


@pytest.fixture
def layout(tmp_path):
    directory = tmp_path / "instance"
    directory.mkdir()
    return Layout(directory)


@pytest.fixture(scope="module")
def instance(tmp_path_factory):
    """One running instance for the read-only observations of this module."""
    with Cleanup() as cleanup:
        directory = tmp_path_factory.mktemp("native") / "instance"
        directory.mkdir()
        layout = Layout(directory)
        layout.initialize()
        running = Instance(layout, cleanup)
        running.wait_ready(HEALTH)
        yield running


@pytest.mark.security
def test_an_image_without_the_mounted_policies_refuses_unowned_login_and_execution():
    docker(
        [
            "run",
            "--rm",
            "--network",
            "none",
            "--read-only",
            "--cap-drop",
            "ALL",
            "--security-opt",
            "no-new-privileges",
            "--user",
            "1000:1000",
            "--tmpfs",
            "/tmp:mode=1777",
            "--env",
            "PYTHONDONTWRITEBYTECODE=1",
            "--env",
            "HERMES_HOME=/tmp/home",
            "--entrypoint",
            PYTHON,
            image(),
            "-c",
            script(
                f"""
                import os
                from hermes_cli._platform_terminal_policy import (
                    enforce_backend, managed_directory)
                from plugins.dashboard_auth.self_hosted import SelfHostedOIDCProvider
                # Environment pins cannot stand in for the mounted owner policy.
                os.environ['HERMES_DASHBOARD_EXPECTED_ISSUER'] = '{ISSUER}'
                os.environ['HERMES_DASHBOARD_EXPECTED_SUBJECT'] = 'owner-a'
                for operation in [
                    lambda: enforce_backend('local'),
                    lambda: enforce_backend('managed_execution'),
                    managed_directory,
                    lambda: SelfHostedOIDCProvider(
                        issuer='{ISSUER}', client_id='native-hermes'),
                ]:
                    try:
                        operation()
                    except (RuntimeError, ValueError):
                        pass
                    else:
                        raise AssertionError('Missing instance policy was accepted')
                """
            ),
        ],
        timeout=30,
    )


def test_the_ssh_client_runs_as_the_pods_numeric_user(layout):
    """OpenSSH rejects numeric users missing from passwd before any network
    connection; the image must run its client as the Pod's UID."""
    docker(
        [
            "run",
            "--rm",
            *layout.common(),
            "--entrypoint",
            "/usr/bin/ssh",
            image(),
            "-G",
            "-F",
            "/dev/null",
            "worker@fixture.invalid",
        ],
        timeout=20,
    )


def test_initialization_leaves_a_complete_receipt_in_the_kubelet_result_file(layout):
    layout.initialize()
    receipt = json.loads(layout.result.read_text())
    assert receipt["record"]["complete"] is True
    assert (layout.root / "home/config.yaml").is_file()
    assert (layout.root / "home/.agent-platform").is_dir()


def test_a_second_initialization_keeps_the_home_and_reports_the_same_receipt(layout):
    layout.initialize()
    first = layout.result.read_bytes()
    (layout.root / "home/owner-memory.txt").write_text("owner fixture memory")
    layout.initialize()
    assert layout.result.read_bytes() == first
    assert (layout.root / "home/owner-memory.txt").read_text() == "owner fixture memory"


def test_managed_values_win_and_the_owner_cannot_redirect_the_policy_directory(
    instance,
):
    instance.check(
        script(
            """
            import json, os, pathlib
            os.environ['HERMES_HOME'] = '/opt/data'
            from hermes_cli.config import load_config
            managed = json.loads(
                pathlib.Path('/run/hermes/execution-ca/config.yaml').read_text())
            effective = load_config()
            assert effective['agent']['max_turns'] == managed['agent']['max_turns']
            from hermes_cli.plugins import PluginContext, PluginManifest
            context = PluginContext(
                PluginManifest(name='managed_execution', key='managed_execution'),
                None)
            assert context.get_config('manager_url') == (
                'https://manager.example.org/execution')
            assert context.get_config('mode') == 'per_session'
            from hermes_cli.managed_scope import get_managed_dir
            os.environ['HERMES_MANAGED_DIR'] = '/tmp/owner-selected-policy'
            os.environ['TERMINAL_ENV'] = 'local'
            assert get_managed_dir() == pathlib.Path('/run/hermes/execution-ca')
            """
        )
    )


@pytest.mark.security
def test_every_terminal_scope_resolves_to_the_managed_backend_and_others_are_refused(
    instance,
):
    instance.check(
        script(
            """
            import os
            os.environ['HERMES_HOME'] = '/opt/data'
            from tools.terminal_scope import (set_terminal_scope,
                reset_terminal_scope, terminal_env, TerminalPolicyRefusal,
                TerminalPolicyUnavailable)
            for scope in [None, {'TERMINAL_ENV': 'local'}, {'TERMINAL_ENV': 'ssh'}]:
                token_scope = set_terminal_scope(scope)
                try:
                    assert terminal_env('TERMINAL_ENV') == 'managed_execution'
                finally:
                    reset_terminal_scope(token_scope)
            token_scope = set_terminal_scope(TerminalPolicyRefusal('fixture'))
            try:
                try:
                    terminal_env('TERMINAL_ENV')
                except TerminalPolicyUnavailable:
                    pass
                else:
                    raise AssertionError('Refused profile scope was bypassed')
            finally:
                reset_terminal_scope(token_scope)
            from tools.terminal_tool_backends import _create_environment
            for backend in ['local', 'docker', 'ssh']:
                try:
                    _create_environment(backend, '', '/workspace', 5)
                except ValueError as error:
                    assert 'require the managed_execution' in str(error)
                else:
                    raise AssertionError('Unmanaged terminal backend was allowed')
            """
        )
    )


@pytest.mark.security
def test_an_owner_plugin_directory_cannot_shadow_the_bundled_terminal_plugin(instance):
    instance.check(
        script(
            """
            import pathlib
            from hermes_cli.plugins_discovery import (collect_directory_manifests,
                resolve_manifest_winners, gate_manifest)
            shadow = pathlib.Path('/opt/data/plugins/managed_execution')
            shadow.mkdir(parents=True, exist_ok=True)
            (shadow / 'plugin.yaml').write_text(
                'name: managed_execution\\nversion: 0.0.1\\n')
            winners = resolve_manifest_winners(collect_directory_manifests())
            pinned = winners['managed_execution']
            assert pinned.source == 'bundled'
            assert pathlib.Path(pinned.path) == pathlib.Path(
                '/opt/hermes/plugins/managed_execution')
            gate = gate_manifest(pinned, {'managed_execution'}, set())
            assert gate.action == 'load' and gate.enabled
            from hermes_cli._platform_terminal_policy import load_policy
            assert load_policy()['terminal']['backend'] == 'managed_execution'
            """
        )
    )


@pytest.mark.security
def test_a_profile_cannot_choose_another_backend_or_redirect_the_connection(instance):
    instance.check(
        script(
            """
            import json, os, pathlib
            from hermes_cli.config import load_config
            managed = json.loads(
                pathlib.Path('/run/hermes/execution-ca/config.yaml').read_text())
            profile = pathlib.Path('/tmp/policy-profile')
            profile.mkdir(exist_ok=True)
            (profile / 'config.yaml').write_text(json.dumps({
                'terminal': {'backend': 'local', 'runtime_id': 'python-tools'},
                'plugins': {'enabled': [], 'disabled': ['managed_execution']}}))
            (profile / '.env').write_text('TERMINAL_ENV=ssh\\n')
            os.environ['HERMES_HOME'] = str(profile)
            selected = load_config()
            assert selected['terminal']['backend'] == 'managed_execution'
            assert selected['terminal']['runtime_id'] == 'python-tools'
            assert 'managed_execution' in selected['plugins']['enabled']
            assert 'managed_execution' not in selected['plugins']['disabled']
            from tools.terminal_scope import (build_profile_terminal_scope,
                set_terminal_scope, reset_terminal_scope, terminal_env)
            scope = build_profile_terminal_scope(profile)
            assert scope['TERMINAL_ENV'] == 'managed_execution'
            token_scope = set_terminal_scope(scope)
            try:
                assert terminal_env('TERMINAL_ENV') == 'managed_execution'
            finally:
                reset_terminal_scope(token_scope)
            import importlib.util
            from types import SimpleNamespace
            spec = importlib.util.spec_from_file_location(
                'policy_fixture_provider',
                '/opt/hermes/plugins/managed_execution/__init__.py')
            module = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(module)
            provider = module.Provider(SimpleNamespace(
                profile_name='research',
                get_config=lambda key, default: {
                    'manager_url': 'https://owner.invalid',
                    'token_file': '/tmp/untrusted-token',
                    'ssh_key': '/tmp/untrusted-key'}.get(key, default)))
            settings = provider.settings()
            connection = managed['plugins']['entries']['managed_execution']['settings']
            for key in ['manager_url', 'token_file', 'ca_file', 'ssh_key']:
                assert settings[key] == connection[key]
            assert settings['runtime_id'] == 'python-tools'
            assert settings['mode'] == 'per_session' and 'profile' not in settings
            """
        )
    )


@pytest.mark.security
def test_the_native_dashboard_requires_the_owner_login_and_reveals_no_session_token(
    instance,
):
    instance.check(
        script(
            """
            import json, pathlib, urllib.error, urllib.request
            response = urllib.request.urlopen(
                'http://127.0.0.1:9119/api/health', timeout=3)
            assert response.status == 200
            body = response.read(2**20)
            token = pathlib.Path(
                '/run/hermes/credentials/dashboard-session-token').read_bytes()
            assert token not in body
            providers = json.load(urllib.request.urlopen(
                'http://127.0.0.1:9119/api/auth/providers', timeout=3))
            assert [p['name'] for p in providers['providers']] == ['self-hosted']
            class NoRedirect(urllib.request.HTTPRedirectHandler):
                def redirect_request(self, *args, **kwargs):
                    return None
            try:
                urllib.request.build_opener(NoRedirect()).open(
                    'http://127.0.0.1:9119/', timeout=3)
            except urllib.error.HTTPError as error:
                assert error.code == 302
                assert error.headers['Location'].startswith(
                    '/auth/login?provider=self-hosted&')
            else:
                raise AssertionError('Native root authentication was bypassed')
            for path in ['/api/auth/me', '/api/sessions']:
                try:
                    urllib.request.urlopen('http://127.0.0.1:9119' + path, timeout=3)
                except urllib.error.HTTPError as error:
                    assert error.code == 401
                else:
                    raise AssertionError('Native authentication was bypassed')
            """
        )
    )


@pytest.mark.security
def test_the_gateway_and_dashboard_run_and_the_initialization_key_is_not_mounted(
    instance,
):
    instance.check(
        script(
            """
            import os, pathlib
            commands = [(p / 'cmdline').read_bytes()
                for p in pathlib.Path('/proc').iterdir()
                if p.name.isdigit() and p.name not in {'1', str(os.getpid())}]
            assert any(b'gateway\\x00run' in c for c in commands)
            assert any(b'dashboard\\x00--host' in c for c in commands)
            assert not pathlib.Path('/run/hermes/init/init-key').exists()
            """
        )
    )


def test_the_provider_trust_bundle_contains_the_system_roots_and_the_fixture_ca(
    instance,
):
    instance.check(
        script(
            """
            import pathlib
            bundles = list(pathlib.Path('/tmp').glob('hermes-ca*.pem'))
            assert bundles
            assert all(p.read_bytes().count(b'BEGIN CERTIFICATE') > 1 for p in bundles)
            """
        )
    )


def test_an_infrastructure_update_and_an_abrupt_kill_keep_the_owners_state(layout):
    with Cleanup() as cleanup:
        layout.initialize()
        running = Instance(layout, cleanup)
        running.wait_ready(HEALTH)
        # Dashboard writes belong to the retained user layer; an infrastructure
        # update overrides only the managed leaves.
        settings_path = layout.root / "home/config.yaml"
        settings = json.loads(settings_path.read_text())
        settings["agent"] = {"max_turns": 77, "fixture_preference": "owner choice"}
        settings_path.write_text(json.dumps(settings))
        layout.managed["agent"]["max_turns"] = 24
        layout.write_managed()
        (layout.root / "home/owner-memory.txt").write_text("owner fixture memory")

        running.kill_and_restart()
        running.wait_ready(HEALTH)
        running.check(
            script(
                """
                import json, os, pathlib
                os.environ['HERMES_HOME'] = '/opt/data'
                from hermes_cli.config import load_config
                effective = load_config()
                assert effective['agent']['max_turns'] == 24
                assert effective['agent']['fixture_preference'] == 'owner choice'
                raw = json.loads(pathlib.Path('/opt/data/config.yaml').read_text())
                assert raw['agent']['max_turns'] == 77
                """
            )
        )
        assert (layout.root / "home/owner-memory.txt").read_text() == (
            "owner fixture memory"
        )


def test_an_instance_whose_supervisor_exits_is_reported_not_waited_for(layout):
    """The readiness helper must fail fast rather than burn its deadline."""
    with Cleanup() as cleanup:
        layout.initialize()
        running = Instance(layout, cleanup)
        running.wait_ready(HEALTH)
        docker(["kill", running.name], timeout=15)
        with pytest.raises(CommandFailed, match="exited before readiness"):
            running.wait_ready(HEALTH, seconds=5)
