"""Native auth uses the same account lifecycle with a one-time PKCE handoff."""
import base64
import hashlib
import os
import re
import time
import unittest
from unittest.mock import patch
from urllib.parse import parse_qs, urlparse

import tests.test_app as base
from social_auth import NATIVE_CODE_TTL
from tests.test_app import app
from database import open_request_connection


class NativeAuthTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        base.WiringTest.setUpClass()

    def setUp(self):
        base.WiringTest.setUp(self)
        self.mobile = app.test_client(use_cookies=False)
        self.browser = app.test_client()
        self.env = patch.dict(os.environ, {'GOOGLE_CLIENT_ID': 'test-client', 'GOOGLE_CLIENT_SECRET': 'test-secret'})
        self.env.start()
        self.addCleanup(self.env.stop)
        self.verifier = 'v' * 64
        self.challenge = base64.urlsafe_b64encode(hashlib.sha256(self.verifier.encode()).digest()).rstrip(b'=').decode()
        self.state = 'native-state-123456789'

    def start(self, **extra):
        return self.mobile.post('/api/auth/google/start', json=dict(mode='register', age_confirm=True,
            privacy_confirm=True, native=True, code_challenge=self.challenge, native_state=self.state, **extra))

    def code(self):
        start = self.start()
        self.assertEqual(start.status_code, 200, start.get_data(as_text=True))
        self.assertNotIn('Set-Cookie', start.headers)
        launch = urlparse(start.get_json()['url'])
        opened = self.browser.get(launch.path + '?' + launch.query)
        self.assertEqual(opened.status_code, 302)
        query = parse_qs(urlparse(opened.location).query)
        with patch('social_auth.exchange_identity', return_value=('subject', 'native@gmail.com', 'Ада', True)):
            callback = self.browser.get('/api/auth/google/callback', query_string=dict(state=query['state'][0], code='provider-code'))
        self.assertEqual(callback.status_code, 302, callback.get_data(as_text=True))
        result = urlparse(callback.location)
        self.assertEqual((result.scheme, result.netloc), ('wiring', 'oauth'))
        data = parse_qs(result.query)
        self.assertEqual(data['state'], [self.state])
        self.assertNotIn('access_token', data)
        self.assertNotIn('refresh_token', data)
        return data['code'][0]

    def exchange(self, code, verifier=None):
        return self.mobile.post('/api/auth/native/exchange', json=dict(code=code, code_verifier=verifier or self.verifier))

    def test_native_pkce_returns_cookie_free_session_and_consumes_code(self):
        code = self.code()
        invalid = self.exchange(code, 'wrong' * 13)
        self.assertEqual(invalid.status_code, 400)
        result = self.exchange(code)
        self.assertEqual(result.status_code, 200, result.get_data(as_text=True))
        self.assertNotIn('Set-Cookie', result.headers)
        self.assertEqual(result.headers['Cache-Control'], 'no-store')
        tokens = result.get_json()
        self.assertEqual(tokens['user']['email'], 'native@gmail.com')
        profile = self.mobile.get('/api/me', headers={'Authorization': 'Bearer ' + tokens['access_token']})
        self.assertEqual(profile.get_json()['user']['id'], tokens['user']['id'])
        self.assertEqual(self.exchange(code).status_code, 400)

    def test_apple_native_handoff_preserves_nonce_and_post_callback(self):
        with patch.dict(os.environ, {'APPLE_CLIENT_ID': 'test-service', 'APPLE_CLIENT_SECRET': 'test-secret'}):
            start = self.mobile.post('/api/auth/apple/start', json=dict(mode='register', age_confirm=True,
                privacy_confirm=True, native=True, code_challenge=self.challenge, native_state=self.state))
            launch = urlparse(start.get_json()['url'])
            opened = self.browser.get(launch.path + '?' + launch.query)
            query = parse_qs(urlparse(opened.location).query)
            self.assertEqual(query['response_mode'], ['form_post'])
            self.assertTrue(query['nonce'][0])
            self.assertNotIn('code_challenge', query)
            posted = self.browser.post('/api/auth/apple/callback', data=dict(state=query['state'][0], code='apple-code'))
            self.assertEqual(posted.status_code, 303)
            with patch('social_auth.exchange_identity', return_value=('apple-subject', 'apple-native@example.com', 'Участник', False)) as identity:
                callback = self.browser.get(posted.location)
            self.assertEqual(identity.call_args.args[3], query['nonce'][0])
            native = parse_qs(urlparse(callback.location).query)
            self.assertEqual(native['state'], [self.state])
            result = self.exchange(native['code'][0])
            self.assertEqual(result.status_code, 200)
            self.assertNotIn('Set-Cookie', result.headers)
            self.assertEqual(self.exchange(native['code'][0]).status_code, 400)

    def test_expired_code_and_arbitrary_callback_are_rejected(self):
        code = self.code()
        conn = open_request_connection()
        conn.execute('UPDATE oauth_native_codes SET created_at = ?', (int(time.time()) - NATIVE_CODE_TTL - 1,))
        conn.commit()
        conn.close()
        self.assertEqual(self.exchange(code).status_code, 400)
        result = self.mobile.post('/api/auth/google/start', json=dict(mode='login', native=True, code_challenge='bad', native_state='bad', redirect_uri='evil://callback'))
        self.assertEqual(result.status_code, 400)

    def test_browser_ticket_and_registration_consents_are_required(self):
        start = self.start()
        url = urlparse(start.get_json()['url'])
        query = parse_qs(url.query)
        bad = self.browser.get(url.path, query_string=dict(state=query['state'][0], ticket='wrong'))
        self.assertEqual(bad.status_code, 400)
        result = self.mobile.post('/api/auth/google/start', json=dict(mode='register', native=True, code_challenge=self.challenge, native_state=self.state))
        self.assertEqual(result.status_code, 400)

    def test_provider_cancellation_returns_to_native_with_state(self):
        start = self.start()
        url = urlparse(start.get_json()['url'])
        opened = self.browser.get(url.path + '?' + url.query)
        state = parse_qs(urlparse(opened.location).query)['state'][0]
        result = self.browser.get('/api/auth/google/callback', query_string=dict(state=state, error='access_denied'))
        query = parse_qs(urlparse(result.location).query)
        self.assertEqual(query['state'], [self.state])
        self.assertIn('error', query)

    def _android_callback(self, user_agent, **query):
        start = self.start()
        url = urlparse(start.get_json()['url'])
        opened = self.browser.get(url.path + '?' + url.query)
        state = parse_qs(urlparse(opened.location).query)['state'][0]
        return self.browser.get('/api/auth/google/callback', query_string=dict(state=state, **query),
                                 headers={'User-Agent': user_agent})

    def _continue(self, callback):
        body = callback.get_data(as_text=True)
        self.assertEqual(callback.status_code, 200, body)
        self.assertNotIn('intent://', body)
        self.assertNotIn('wiring://', body)
        self.assertNotIn('<script', body)
        self.assertNotIn('http-equiv="refresh"', body)
        fields = dict(re.findall(r'name="([^"]+)" value="([^"]*)"', body))
        opened = self.browser.post('/api/auth/native/open', data=fields)
        page = opened.get_data(as_text=True)
        self.assertEqual(opened.status_code, 200, page)
        self.assertNotIn('intent://', page)
        self.assertNotIn('wiring://', page)
        self.assertNotIn('access_token', page)
        self.assertNotIn('refresh_token', page)
        launch = dict(re.findall(r'name="([^"]+)" value="([^"]*)"', page))
        self.assertEqual(launch.get('launch'), '1')
        handed = self.browser.post('/api/auth/native/open', data=launch)
        self.assertEqual(handed.status_code, 302, handed.get_data(as_text=True))
        location = handed.headers['Location']
        self.assertIn('#Intent;scheme=wiring;package=date.wiring.app;', location)
        intent = urlparse(location)
        self.assertEqual((intent.scheme, intent.netloc, intent.path), ('intent', 'oauth', '/'))
        return parse_qs(intent.query)

    def test_android_chrome_return_is_a_tap_not_a_scheme_redirect(self):
        agents = (
            'Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/157.0.0.0 Safari/537.36',
            'Mozilla/5.0 (Linux; Android 10; K) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/157.0.0.0 Mobile Safari/537.36',
        )
        for user_agent in agents:
            with patch('social_auth.exchange_identity', return_value=('subject', 'native@gmail.com', 'Ада', True)):
                callback = self._android_callback(user_agent, code='provider-code')
            data = self._continue(callback)
            self.assertEqual(data['state'], [self.state])
            self.assertEqual(self.exchange(data['code'][0]).status_code, 200)

        cancelled = self._android_callback(agents[0], error='access_denied')
        cancel_query = self._continue(cancelled)
        self.assertEqual(cancel_query['state'], [self.state])
        self.assertIn('error', cancel_query)
        self.assertNotIn('code', cancel_query)
        self.assertEqual(self.browser.post('/api/auth/native/open', data={'state': 'nope'}).status_code, 400)

        with patch('social_auth.exchange_identity', return_value=('subject', 'ios-native@gmail.com', 'Ада', True)):
            ios = self._android_callback(
                'Mozilla/5.0 (iPhone; CPU iPhone OS 18_7 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/27.0.1 Mobile/15E148 Safari/604.1',
                code='provider-code')
        self.assertEqual(ios.status_code, 302)
        self.assertTrue(ios.location.startswith('wiring://oauth?'))

    def test_email_verification_issues_native_tokens_once(self):
        # Registration's normal verification proof, no production mail/account access.
        with patch('app.email_verify_enforced', return_value=True), patch('app.send_mail'):
            result = self.mobile.post('/api/register', json=dict(name='Ада', email='verify-native@example.com', password='secret1', age_confirm=True, privacy_confirm=True))
        self.assertEqual(result.status_code, 200, result.get_data(as_text=True))
        conn = open_request_connection()
        proof = conn.execute('SELECT token FROM email_verifications ORDER BY created_at DESC LIMIT 1').fetchone()
        conn.close()
        self.assertIsNotNone(proof)
        verified = self.mobile.post('/api/email/verify', json=dict(token=proof['token'], mobile=True))
        self.assertEqual(verified.status_code, 200, verified.get_data(as_text=True))
        self.assertIn('access_token', verified.get_json())
        self.assertNotIn('Set-Cookie', verified.headers)
        self.assertEqual(self.mobile.post('/api/email/verify', json=dict(token=proof['token'], mobile=True)).status_code, 400)

    def test_app_links_use_known_app_identity_and_configured_release_fingerprints(self):
        apple = self.mobile.get('/.well-known/apple-app-site-association')
        self.assertEqual(apple.status_code, 200)
        detail = apple.get_json()['applinks']['details'][0]
        self.assertEqual(detail['appIDs'], ['G3T7684N3M.date.wiring.app'])
        self.assertIn({'/': '/api/*', 'exclude': True}, detail['components'])
        with patch.dict(os.environ, {'ANDROID_APP_LINK_SHA256': ''}):
            self.assertEqual(self.mobile.get('/.well-known/assetlinks.json').get_json(), [])
        fingerprint = ':'.join(['AA'] * 32)
        with patch.dict(os.environ, {'ANDROID_APP_LINK_SHA256': fingerprint + ',invalid'}):
            android = self.mobile.get('/.well-known/assetlinks.json').get_json()[0]['target']
        self.assertEqual(android['package_name'], 'date.wiring.app')
        self.assertEqual(android['sha256_cert_fingerprints'], [fingerprint])

    def test_support_accepts_native_bearer_without_cookie(self):
        code = self.code()
        tokens = self.exchange(code).get_json()
        with patch('app.notify_support'):
            result = self.mobile.post('/support', json=dict(body='Помогите с моей анкетой'),
                headers={'Authorization': 'Bearer ' + tokens['access_token']})
        self.assertEqual(result.status_code, 200)
        self.assertNotIn('Set-Cookie', result.headers)
        conn = open_request_connection()
        ticket = conn.execute('SELECT user_id FROM support_tickets ORDER BY id DESC LIMIT 1').fetchone()
        conn.close()
        self.assertEqual(ticket['user_id'], tokens['user']['id'])
