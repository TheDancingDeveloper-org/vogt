// One build of the merged stack (WI-1649).
//
// `stack` is pushed by digest, with provenance and an SBOM, and no tag.
// build.yml smokes the digest, and only then tags and signs. A second or third
// docker/build-push-action would rebuild the graph this file builds once.

variable "STACK_IMAGE" {
  default = ""
}

variable "CACHE_REF" {
  default = ""
}

variable "POD_BASE_IMAGE" {
  default = ""
}

variable "ESTATE_IMAGE" {
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
    ESTATE_IMAGE         = ESTATE_IMAGE
    VOGT_SOURCE_REF      = VOGT_SOURCE_REF
    VOGT_SOURCE_SHA      = VOGT_SOURCE_SHA
    VOGT_PRODUCT_VERSION = VOGT_PRODUCT_VERSION
    INSTALL_AI_CLIENTS   = "true"
  }
  labels = {
    "org.opencontainers.image.revision" = VOGT_SOURCE_SHA
  }
  output = ["type=image,name=${STACK_IMAGE},push-by-digest=true,name-canonical=true,push=true"]
  # mode=min matches docker/build-push-action's provenance default.
  # mode=max would record build args, which the previous push did not.
  attest = [
    "type=provenance,mode=min",
    "type=sbom",
  ]
  cache-from = ["type=registry,ref=${CACHE_REF}"]
  cache-to   = ["type=registry,ref=${CACHE_REF},mode=max"]
}

