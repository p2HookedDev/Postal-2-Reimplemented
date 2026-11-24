// No preset writes these rows, and changing one keeps the preset.
class ModernMenuParticles extends ModernMenuPage;

var localized string    FireNames[3];

function CreateMenuContents()
{
	local UWindowComboControl Combo;
	local int i;

	Super.CreateMenuContents();

	AddTitle(TitleText, TitleFont, TitleAlign);

	bUpdate = false;
	AddBox(0, "ModernParticleLight");
	AddLevelSlider(1, "ModernSoftParticles", 0, 10, 5);
	AddBox(2, "ModernDecalAging");
	AddBox(3, "ModernWounds");
	AddBox(4, "ModernLiquidFlow");
	AddBox(5, "ModernFireLight");
	AddLevelSlider(6, "ModernGpuParticles", 0, 10, 5);
	AddBack();
	LoadRows();
}

function RowChanged(int i)
{
}

defaultproperties
{
	TitleText="Blood and Particles"
	BackHelp="Return to the Modern Graphics menu."

	FireNames(0)="Original"
	FireNames(1)="Coarse"
	FireNames(2)="Fine"

	Labels(0)="Particle lighting"
	Labels(1)="Soft particles"
	Labels(2)="Drying blood"
	Labels(3)="Blood on bodies"
	Labels(4)="Puddles and streams"
	Labels(5)="Fire and muzzle flash light"
	Labels(6)="GPU particles"

	Helps(0)="Smoke and other particles are lit by the room, the sun and nearby lamps."
	Helps(1)="Particles fade where they touch walls and floors instead of cutting into them."
	Helps(2)="Blood dries over time, and old marks fade out instead of vanishing."
	Helps(3)="Blood stays on a body where it was hit."
	Helps(4)="Smoother streams and uneven puddle edges."
	Helps(5)="Fire and gunshots light up nearby people, walls and smoke."
	Helps(6)="Extra blood, sparks and debris that bounce off surfaces. Higher adds more."
}
