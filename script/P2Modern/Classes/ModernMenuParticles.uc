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
	Combo = AddLevelCombo(5, "ModernFireVolumes", 1, 2);
	for (i = 0; i < ArrayCount(FireNames); i++)
		Combo.AddItem(FireNames[i]);
	AddBack();
	LoadRows();
}

function RowChanged(int i)
{
}

defaultproperties
{
	TitleText="Fire, Blood and Particles"
	BackHelp="Return to the Modern Graphics menu."

	FireNames(0)="Original"
	FireNames(1)="Coarse"
	FireNames(2)="Fine"

	Labels(0)="Lit particles"
	Labels(1)="Soft particles"
	Labels(2)="Drying blood"
	Labels(3)="Wounds"
	Labels(4)="Flowing liquids"
	Labels(5)="Fire"

	Helps(0)="Smoke, sparks and dust take the light and shadow around them."
	Helps(1)="Particles fade where they meet a wall or floor instead of cutting into it. 0 turns it off."
	Helps(2)="Blood dries over time, and marks fade out instead of vanishing."
	Helps(3)="Blood stays on a body where it was hit."
	Helps(4)="Poured liquids run as streams and spread into puddles."
	Helps(5)="Original draws flat flames. Coarse and Fine draw fire as moving volume; Fine costs more."
}
