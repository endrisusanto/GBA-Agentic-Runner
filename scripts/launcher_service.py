import subprocess
from http.server import HTTPServer, BaseHTTPRequestHandler
import json

class LauncherHandler(BaseHTTPRequestHandler):
    def do_POST(self):
        if self.path == '/start-runner':
            try:
                # Run the ensure-runner.sh script
                script_path = "/run/media/endri-pro/BINARY_HDD/AUTO/scripts/ensure-runner.sh"
                result = subprocess.run([script_path], capture_output=True, text=True, check=True)
                
                self.send_response(200)
                self.send_header('Content-Type', 'application/json')
                self.end_headers()
                response = {
                    "status": "success",
                    "output": result.stdout.strip()
                }
                self.wfile.write(json.dumps(response).encode('utf-8'))
            except subprocess.CalledProcessError as e:
                self.send_response(500)
                self.send_header('Content-Type', 'application/json')
                self.end_headers()
                response = {
                    "status": "error",
                    "message": str(e),
                    "output": e.stdout.strip() if e.stdout else "",
                    "error": e.stderr.strip() if e.stderr else ""
                }
                self.wfile.write(json.dumps(response).encode('utf-8'))
        else:
            self.send_response(404)
            self.end_headers()

    def do_GET(self):
        # Health check
        if self.path == '/status':
            self.send_response(200)
            self.send_header('Content-Type', 'application/json')
            self.end_headers()
            self.wfile.write(json.dumps({"status": "ready"}).encode('utf-8'))
        else:
            self.send_response(404)
            self.end_headers()

def run(server_class=HTTPServer, handler_class=LauncherHandler, port=3031):
    server_address = ('0.0.0.0', port)
    httpd = server_class(server_address, handler_class)
    print(f"Starting GBA Launcher Service on port {port}...")
    try:
        httpd.serve_forever()
    except KeyboardInterrupt:
        pass
    print("Stopping server...")

if __name__ == '__main__':
    run()
