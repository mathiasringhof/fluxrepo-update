pod = {"kind": "Pod", "spec": {"containers": [{"name": "probe", "image": "example/busybox:1.37.0"}]}}
other = {"kind": "Pod", "spec": {"containers": [{"name": "probe", "image": "example/python:3.12-slim-bookworm@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}]}}
raise RuntimeError("This source must never be executed by discovery")
