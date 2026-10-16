// One build of the workbench stack (WI-1650).
//
// The same shape as `.github/stack-bake.hcl` (WI-1649), kept as its own file
// so the two land independently. `stack` is pushed by digest, with provenance
// and an SBOM, and no tag. build.yml smokes the digest and only then tags and
// signs. There is no gitguard target: the lean stack's bake already builds
// that binary from the same Dockerfile, and running it twice proves nothing.

variable "STACK_WORKBENCH_IMAGE" {
  default = ""
}

variable "CACHE_REF" {
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
  output = ["type=image,name=${STACK_WORKBENCH_IMAGE},push-by-digest=true,name-canonical=true,push=true"]
  attest = [
    "type=provenance,mode=min",
    "type=sbom",
  ]
  cache-from = ["type=registry,ref=${CACHE_REF}"]
  cache-to   = ["type=registry,ref=${CACHE_REF},mode=max"]
}
