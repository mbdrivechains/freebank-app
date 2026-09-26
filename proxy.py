#!/usr/bin/env python3
"""
CORS proxy for developing the FreeBank app as a web page (PWA).

A browser page can't send JSON-RPC to freebankd directly, because the node sends no CORS headers.
This proxy forwards the page's requests to the node and adds those headers. It is deliberately narrow:

- it listens on 127.0.0.1 only (unless --bind says otherwise);
- it answers only the given origin (the Vite dev server by default), never '*';
- it adds no login of its own: it passes on the Authorization header the page sends, so the page
  still needs the node's RPC credentials.

Usage:
    python3 proxy.py --rpc-port 8454          # main; 18457 for regtest

Then point the page at http://127.0.0.1:3001.
"""

import argparse
import json
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, HTTPServer


class CORSProxyHandler(BaseHTTPRequestHandler):
    rpc_url = None
    origin = None

    def send_cors_headers(self):
        self.send_header('Access-Control-Allow-Origin', self.origin)
        self.send_header('Vary', 'Origin')
        self.send_header('Access-Control-Allow-Methods', 'POST, OPTIONS')
        self.send_header('Access-Control-Allow-Headers', 'Content-Type, Authorization')

    def origin_ok(self):
        return self.headers.get('Origin') == self.origin

    def do_OPTIONS(self):
        """Preflight: only the allowed origin gets a yes."""
        self.send_response(204 if self.origin_ok() else 403)
        if self.origin_ok():
            self.send_cors_headers()
        self.end_headers()

    def do_POST(self):
        """Forward one RPC request to freebankd, with the page's own login."""
        if not self.origin_ok():
            self.send_response(403)
            self.end_headers()
            return
        try:
            body = self.rfile.read(int(self.headers.get('Content-Length', 0)))
            headers = {'Content-Type': 'application/json'}
            if self.headers.get('Authorization'):
                headers['Authorization'] = self.headers['Authorization']
            req = urllib.request.Request(self.rpc_url, data=body, headers=headers, method='POST')
            with urllib.request.urlopen(req, timeout=30) as response:
                result = response.read()
            self.send_response(200)
            self.send_header('Content-Type', 'application/json')
            self.send_cors_headers()
            self.end_headers()
            self.wfile.write(result)
        except urllib.error.HTTPError as e:
            self.send_response(e.code)
            self.send_cors_headers()
            self.end_headers()
            self.wfile.write(e.read())
        except Exception as e:
            self.send_response(502)
            self.send_cors_headers()
            self.end_headers()
            self.wfile.write(json.dumps({'error': str(e)}).encode())

    def log_message(self, format, *args):
        print(f"[proxy] {args[0]}")


def main():
    parser = argparse.ArgumentParser(description='CORS proxy for FreeBank RPC (local development)')
    parser.add_argument('--bind', default='127.0.0.1', help='address to listen on (default: 127.0.0.1)')
    parser.add_argument('--port', type=int, default=3001, help='proxy port (default: 3001)')
    parser.add_argument('--origin', default='http://localhost:5173', help='the page origin allowed to use it')
    parser.add_argument('--rpc-host', default='127.0.0.1', help='freebankd host')
    parser.add_argument('--rpc-port', type=int, default=8454, help='freebankd RPC port (main 8454, regtest 18457)')
    args = parser.parse_args()

    CORSProxyHandler.rpc_url = f'http://{args.rpc_host}:{args.rpc_port}/'
    CORSProxyHandler.origin = args.origin

    server = HTTPServer((args.bind, args.port), CORSProxyHandler)
    print(f'CORS proxy on http://{args.bind}:{args.port} for {args.origin}, forwarding to {CORSProxyHandler.rpc_url}')
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        server.shutdown()


if __name__ == '__main__':
    main()
