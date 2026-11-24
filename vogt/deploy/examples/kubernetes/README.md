# Kubernetes orchestrator example (rust-2)

Read [the deployment chapter](../../../docs/DEPLOYMENT.md#kubernetes-orchestrator-rust-2)
before applying. This is a template, not a tested deployment receipt.

1. Copy `stack.yaml` into your private deployment repository. Replace every
   `REPLACE_*` value. Build engine and session images from the same tested
   `rust-2` revision; pin both by digest. Configure registry pull secrets in
   both namespaces when needed.
2. Choose a storage class and node placement. Ensure `/var/lib/vogt` is
   writable by the image's runtime user. Configure Claude account directories
   on this PVC if using Claude; provide agent/provider credentials through
   the engine's supported bootstrap configuration. The template does not
   include a model-provider login or a voice service.
3. Create the namespaces, then provision `engine-credentials` in `vogt` using
   your secret manager: keys `core-token` and `graft-mint-token`. Never commit
   values. The mint credential must allow the selected repository and line.
4. Extend `session-isolation` with narrowly scoped egress to your graft server,
   model providers, approved MCP services and package sources/proxy. DNS
   selectors must match your cluster. No general Internet allowance is supplied:
   cloning will fail until you add the graft path. NetworkPolicy needs an
   enforcing CNI; it cannot filter HTTPS by hostname without an egress gateway.
5. Review the internal plaintext HTTP choice. It is explicit and confined to
   the cluster path; use HTTPS and remove the insecure flag if your trust
   model requires encryption. Add your TLS ingress and exact allowed origins
   deliberately; no public ingress is created by this example.
   In `stack.yaml`, change `ENGINE_POD_ALLOW_INSECURE_ENDPOINT=true` and the
   `ENGINE_POD_ENGINE_ID: example-engine` placeholder for your environment.
6. Run `kubectl apply --dry-run=server -f stack.yaml` on the configured copy,
   inspect admission/RBAC/quota results, then apply after operator approval.
   Check `/readyz`, install/login, start a session and confirm cloning and
   authenticated streaming. Test hibernate/wake and engine restart separately;
   a running pod is not re-adopted after engine restart today.

The engine identity can create/get/list/delete session pods and create their
one-use bootstrap Secrets. It cannot read Secrets or exec/attach into pods.
Secret owner references let Kubernetes garbage-collect them with the pod.
The session ServiceAccount has no RoleBinding and no mounted API token.
Restricted Pod Security is compatible with the default cache-free pod spec;
hostPath seeds need a separately reviewed admission exception, not a blanket
relaxation. Adjust quota, priorities and resources to your cluster.
