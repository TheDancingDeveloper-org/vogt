// One build of the merged stack (WI-1649).
//
// `stack` is pushed by digest, with provenance and an SBOM, and no tag.
// `gitguard` is the Codex gitguard e2e binary from the same BuildKit graph,
// written to a local directory. build.yml smokes the digest, runs the binary
// inside it, and only then tags and signs. A second or third
// docker/build-push-action would rebuild the graph this file builds once.

variable "STACK_IMAGE" {
  default = ""
}

variable "CACHE_REF" {
  default = ""
}

variable "E2E_DIR" {
  default = ""
}

variable "POD_BASE_IMAGE" {
  default = ""
}

variable "CORE_IMAGE" {
  default = ""
}

variable "VOGT_SOURCE_REF" {
  default = ""
}

variable "VOGT_SOURCE_SHA" {
  default = ""
}

variable "VOGT_PRODUCT_VERSION" {
  default = ""
}

variable "LABELS" {
  default = ""
}

target "stack" {
  context    = "."
  dockerfile = "engine/Dockerfile"
  args = {
    POD_BASE_IMAGE       = POD_BASE_IMAGE
    CORE_IMAGE           = CORE_IMAGE
    VOGT_SOURCE_REF      = VOGT_SOURCE_REF
    VOGT_SOURCE_SHA      = VOGT_SOURCE_SHA
    VOGT_PRODUCT_VERSION = VOGT_PRODUCT_VERSION
    INSTALL_AI_CLIENTS   = "true"
  }
  labels = {
    "org.opencontainers.image.revision" = VOGT_SOURCE_SHA
  }
  output = ["type=image,name=${STACK_IMAGE},push-by-digest=true,name-canonical=true,push=true"]
  attest = [
    "type=provenance,mode=max",
    "type=sbom",
  ]
  cache-from = ["type=registry,ref=${CACHE_REF}"]
  cache-to   = ["type=registry,ref=${CACHE_REF},mode=max"]
}

target "gitguard" {
  context    = "."
  dockerfile = "engine/Dockerfile"
  target     = "codex-gitguard-e2e"
  args = {
    POD_BASE_IMAGE       = POD_BASE_IMAGE
    CORE_IMAGE           = CORE_IMAGE
    VOGT_SOURCE_REF      = VOGT_SOURCE_REF
    VOGT_SOURCE_SHA      = VOGT_SOURCE_SHA
    VOGT_PRODUCT_VERSION = VOGT_PRODUCT_VERSION
    INSTALL_AI_CLIENTS   = "true"
  }
  output     = ["type=local,dest=${E2E_DIR}"]
  cache-from = ["type=registry,ref=${CACHE_REF}"]
}
