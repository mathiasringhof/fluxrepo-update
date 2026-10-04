INSTALL = """
curl -o k3s {{server}}/example/k3s/releases/download/v1.35.8%2Bk3s1/k3s
curl -o sums {{server}}/example/k3s/releases/download/v1.35.8%2Bk3s1/sha256sum-amd64.txt
sha256sum -c sums
"""
EXPECTED_VERSION = "v1.35.8+k3s1"
