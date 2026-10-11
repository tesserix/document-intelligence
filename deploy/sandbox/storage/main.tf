terraform {
  required_version = ">= 1.6.0"
  required_providers {
    google = {
      source  = "hashicorp/google"
      version = "~> 7.0"
    }
  }
}

variable "project_id" {
  type = string
}
variable "location" {
  type = string
}
variable "bucket_suffix" {
  description = "Globally unique deployment suffix; do not reuse production buckets."
  type        = string
  validation {
    condition     = can(regex("^[a-z0-9][a-z0-9-]{2,35}$", var.bucket_suffix))
    error_message = "Use 3–36 lowercase letters, digits or hyphens."
  }
}
variable "cleanup_member" {
  description = "Dedicated workload IAM principal, e.g. serviceAccount:...; no credentials."
  type        = string
}

resource "google_storage_bucket" "sandbox" {
  for_each                    = toset(["quarantine", "source", "pages", "results"])
  project                     = var.project_id
  name                        = "ocr-sandbox-${var.bucket_suffix}-${each.key}"
  location                    = var.location
  uniform_bucket_level_access = true
  public_access_prevention    = "enforced"
  force_destroy               = false
  versioning {
    enabled = false
  }
  soft_delete_policy {
    retention_duration_seconds = 0
  }
  lifecycle_rule {
    condition {
      age = 1
    }
    action {
      type = "Delete"
    }
  }
  lifecycle {
    prevent_destroy = true
  }
}

resource "google_project_iam_custom_role" "cleanup" {
  project     = var.project_id
  role_id     = "ocrSandboxCleanup"
  title       = "OCR sandbox verified cleanup"
  permissions = ["storage.buckets.get", "storage.objects.list", "storage.objects.delete"]
}

resource "google_storage_bucket_iam_member" "cleanup" {
  for_each = google_storage_bucket.sandbox
  bucket   = each.value.name
  role     = google_project_iam_custom_role.cleanup.name
  member   = var.cleanup_member
}

output "sandbox_buckets" {
  value = join(",", [for purpose in ["quarantine", "source", "pages", "results"] : google_storage_bucket.sandbox[purpose].name])
}
