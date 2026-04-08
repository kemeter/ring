use std::path::{Path, PathBuf};
use tokio::process::Command;

/// Convert a Docker image to a rootfs ext4 disk image for Cloud Hypervisor.
///
/// Strategy:
/// 1. Pull the Docker image (if not already present)
/// 2. Create a temporary container from the image
/// 3. Export the container filesystem as a tar
/// 4. Create an ext4 disk image and extract the tar into it
/// 5. Clean up the temporary container
///
/// The resulting rootfs is cached at `<rootfs_dir>/<safe_image_name>.img`.
pub(crate) async fn ensure_rootfs(
    image: &str,
    rootfs_dir: &str,
) -> Result<PathBuf, String> {
    let safe_name = image.replace(['/', ':'], "_");
    let rootfs_path = PathBuf::from(rootfs_dir).join(format!("{}.img", safe_name));

    if rootfs_path.exists() {
        debug!("Rootfs already cached at {:?}", rootfs_path);
        return Ok(rootfs_path);
    }

    tokio::fs::create_dir_all(rootfs_dir)
        .await
        .map_err(|e| format!("Failed to create rootfs dir: {}", e))?;

    info!("Converting Docker image '{}' to rootfs...", image);

    // Step 1: Pull image
    run_command("docker", &["pull", image]).await?;

    // Step 2: Create temporary container
    let container_id = run_command_output("docker", &["create", image])
        .await?
        .trim()
        .to_string();

    // Step 3: Export as tar
    let tar_path = PathBuf::from(rootfs_dir).join(format!("{}.tar", safe_name));
    let tar_str = tar_path.to_str().unwrap_or_default();

    let export_result = run_command("docker", &["export", "-o", tar_str, &container_id]).await;

    // Clean up container regardless of export result
    let _ = run_command("docker", &["rm", &container_id]).await;

    export_result?;

    // Step 4: Create ext4 image from tar
    create_ext4_from_tar(&tar_path, &rootfs_path).await?;

    // Clean up tar
    let _ = tokio::fs::remove_file(&tar_path).await;

    info!("Rootfs created at {:?}", rootfs_path);
    Ok(rootfs_path)
}

async fn create_ext4_from_tar(tar_path: &Path, rootfs_path: &Path) -> Result<(), String> {
    let rootfs_str = rootfs_path.to_str().unwrap_or_default();
    let tar_str = tar_path.to_str().unwrap_or_default();

    // Create a 2GB sparse file
    run_command(
        "dd",
        &[
            "if=/dev/zero",
            &format!("of={}", rootfs_str),
            "bs=1M",
            "count=0",
            "seek=2048",
        ],
    )
    .await?;

    // Format as ext4
    run_command("mkfs.ext4", &["-F", rootfs_str]).await?;

    // Mount and extract tar
    let mount_dir = format!("{}.mnt", rootfs_str);
    tokio::fs::create_dir_all(&mount_dir)
        .await
        .map_err(|e| format!("Failed to create mount dir: {}", e))?;

    run_command("mount", &["-o", "loop", rootfs_str, &mount_dir]).await?;

    let extract_result = run_command("tar", &["-xf", tar_str, "-C", &mount_dir]).await;

    // Always unmount
    let _ = run_command("umount", &[&mount_dir]).await;
    let _ = tokio::fs::remove_dir(&mount_dir).await;

    extract_result?;

    Ok(())
}

async fn run_command(cmd: &str, args: &[&str]) -> Result<(), String> {
    let output = Command::new(cmd)
        .args(args)
        .output()
        .await
        .map_err(|e| format!("Failed to run {} {:?}: {}", cmd, args, e))?;

    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!("{} {:?} failed: {}", cmd, args, stderr))
    }
}

async fn run_command_output(cmd: &str, args: &[&str]) -> Result<String, String> {
    let output = Command::new(cmd)
        .args(args)
        .output()
        .await
        .map_err(|e| format!("Failed to run {} {:?}: {}", cmd, args, e))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!("{} {:?} failed: {}", cmd, args, stderr))
    }
}
